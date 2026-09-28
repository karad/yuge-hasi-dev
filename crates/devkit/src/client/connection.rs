use super::process::{Runner, SystemRunner};
use crate::{
    Build, Result, failure::at, invalid, path_text, quote, regular, validate_host, validate_login,
};
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

// Fixed SSH arguments that ignore user configuration and require pinned host keys.
// Credential paths are added by `ssh_args` for the selected client directory.
const SSH_OPTIONS: [&str; 14] = [
    "-F",
    "/dev/null",
    "-o",
    "BatchMode=yes",
    "-o",
    "IdentitiesOnly=yes",
    "-o",
    "IdentityAgent=none",
    "-o",
    "StrictHostKeyChecking=yes",
    "-o",
    "GlobalKnownHostsFile=/dev/null",
    "-o",
    "ConnectTimeout=5",
];

/// Builds the SSH arguments that isolate Devkit connections from local SSH settings.
pub fn ssh_args(config: &Path) -> Result<Vec<String>> {
    let mut args = SSH_OPTIONS
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    args.extend([
        "-o".into(),
        format!(
            "UserKnownHostsFile={}",
            path_text(&config.join("known_hosts"))?
        ),
        "-o".into(),
        "ServerAliveInterval=5".into(),
        "-o".into(),
        "ServerAliveCountMax=2".into(),
        "-i".into(),
        path_text(&config.join("devkit_rsa"))?.into(),
    ]);
    Ok(args)
}

/// Connection settings and command runner for one SteamOS device.
pub struct Connection<'a> {
    /// Directory containing the SSH identity and pinned host keys.
    pub config: &'a Path,
    /// Private IPv4 address of the device.
    pub host: &'a str,
    /// SSH login on the device.
    pub login: &'a str,
    /// Command runner used for SSH and file transfers.
    pub runner: &'a dyn Runner,
}

impl<'a> Connection<'a> {
    /// Creates a connection that executes commands through the operating system.
    pub fn new(config: &'a Path, host: &'a str, login: &'a str) -> Self {
        Self {
            config,
            host,
            login,
            runner: &SystemRunner,
        }
    }
}

impl Connection<'_> {
    /// Verifies destination syntax, client identity, and pinned host presence.
    pub fn check(&self) -> Result<()> {
        validate_host(self.host)?;
        validate_login(self.login)?;
        regular(&self.config.join("devkit_rsa"))?;
        let known = fs::read_to_string(self.config.join("known_hosts"))?;
        if !known
            .lines()
            .any(|line| line.split_whitespace().next() == Some(self.host))
        {
            return Err(invalid("Verify the host key with trust-host first"));
        }
        Ok(())
    }

    /// Runs a command over SSH after validating the connection settings.
    pub fn ssh(&self, command: &str, input: &[u8]) -> Result<Vec<u8>> {
        self.check()?;
        self.runner.run(
            Command::new("/usr/bin/ssh")
                .args(ssh_args(self.config)?)
                .arg(format!("{}@{}", self.login, self.host))
                .arg(command),
            input,
        )
    }

    /// Fetches the device's remote status response.
    pub fn status(&self) -> Result<Value> {
        self.remote("status", None)
    }

    // Runs a remote operation and rejects responses from another protocol version.
    fn remote(&self, operation: &str, build: Option<&Build>) -> Result<Value> {
        let script = crate::remote::script(operation, build)?;
        let output = self.ssh(crate::remote::COMMAND, script.as_bytes())?;
        let value: Value = serde_json::from_slice(&output)?;
        if value.get("protocol") != Some(&json!(2)) {
            return Err(invalid("Unsupported remote protocol"));
        }
        Ok(value)
    }

    /// Copies a file or directory to a validated absolute remote path.
    pub fn rsync(&self, source: &Path, destination: &str, folder: bool) -> Result<()> {
        self.check()?;
        if !destination.starts_with('/') || destination.chars().any(char::is_control) {
            return Err(invalid("Invalid remote destination"));
        }
        // rsync passes -e through a shell, so each SSH argument needs its own quoting.
        let shell = std::iter::once("/usr/bin/ssh".to_owned())
            .chain(ssh_args(self.config)?)
            .map(|argument| quote(&argument))
            .collect::<Vec<_>>()
            .join(" ");
        let source = format!("{}{}", path_text(source)?, if folder { "/" } else { "" });
        self.runner.run(
            Command::new("/usr/bin/rsync")
                .args([
                    "-a",
                    "--checksum",
                    "--chmod=Du=rwx,Dgo=,Fu=rwx,Fgo=",
                    "-e",
                    &shell,
                    "--",
                    &source,
                ])
                .arg(format!(
                    "{}@{}:{}",
                    self.login,
                    self.host,
                    quote(destination)
                )),
            b"",
        )?;
        Ok(())
    }

    /// Checks Steam readiness and returns the remote home directory.
    pub fn ready(&self) -> Result<String> {
        self.ensure_ready(None)
    }

    /// Checks Steam readiness and the architecture required by a build.
    pub fn ready_for(&self, build: &Build) -> Result<String> {
        self.ensure_ready(Some(build.expected_architecture()))
    }

    // Applies shared readiness checks, optionally including the build architecture.
    fn ensure_ready(&self, expected_architecture: Option<&str>) -> Result<String> {
        let state = self
            .status()
            .map_err(|error| at("connection", "connection_failed", error))?;
        // Check architecture first to report a mismatched build before session readiness.
        if let Some(expected) = expected_architecture
            && state.get("architecture").and_then(Value::as_str) != Some(expected)
        {
            return Err(at(
                "readiness",
                "architecture_mismatch",
                invalid("Build runtime does not match device architecture"),
            ));
        }
        if state.get("inhibit_sentinel_present") != Some(&json!(false)) {
            return Err(at(
                "readiness",
                "session_inhibited",
                invalid("Review the existing session-tracker sentinel before upload"),
            ));
        }
        if state.get("steam_ready") != Some(&json!(true)) {
            return Err(at(
                "readiness",
                "steam_not_ready",
                invalid("Steam IPC is not ready"),
            ));
        }
        crate::remote::decode_home(&state)
            .map_err(|error| at("readiness", "invalid_remote_home", error))
    }

    /// Validates, transfers, and registers a build on the device.
    pub fn upload(&self, build: &Build) -> Result<Value> {
        build
            .validate_source()
            .map_err(|error| at("build", "invalid_build", error))?;
        let home = self.ready_for(build)?;
        self.prepare(build)?;
        // Do not register a title until its transfer has completed successfully.
        self.rsync(
            &build.source,
            &format!("{home}/devkit-game/{}/", build.title),
            build.runtime != "android",
        )
        .map_err(|error| at("transfer", "transfer_failed", error))?;
        self.register(build)?;
        Ok(json!({
            "registered": true,
            "title": build.title,
            "launch_verified": false,
            "helper_required": false
        }))
    }

    // Requires the device to confirm it prepared the title for transfer.
    fn prepare(&self, build: &Build) -> Result<()> {
        let prepared = self
            .remote("prepare", Some(build))
            .map_err(|error| at("prepare", "prepare_failed", error))?;
        if prepared.get("prepared") != Some(&json!(true)) {
            return Err(at(
                "prepare",
                "prepare_failed",
                invalid("Device did not prepare the title"),
            ));
        }
        Ok(())
    }

    // Requires Steam to confirm registration after the transfer completes.
    fn register(&self, build: &Build) -> Result<()> {
        let result = self
            .remote("register", Some(build))
            .map_err(|error| at("register", "registration_failed", error))?;
        if result.get("registered") != Some(&json!(true)) {
            return Err(at(
                "register",
                "registration_failed",
                invalid("Steam did not confirm registration"),
            ));
        }
        Ok(())
    }
}
