use crate::{
    Build, Result,
    client::{self, Connection, Runner},
    failure::at,
    invalid, lock, project, shared,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Resolved inputs for a device deployment or readiness check.
pub struct Deployment {
    /// Registered device name.
    pub device: String,
    /// Device's private network address.
    pub host: String,
    /// SSH account used on the device.
    pub login: String,
    /// Directory containing the client identity and pinned host keys.
    pub auth_dir: PathBuf,
    /// Validated build to check or deploy.
    pub build: Build,
    /// Whether to check readiness without transferring the build.
    pub check_only: bool,
}

impl Deployment {
    /// Resolves a device and build from project or shared settings.
    pub fn resolve(
        path: &Path,
        device: &str,
        source: Option<&Path>,
        check_only: bool,
        game_stopped: bool,
    ) -> Result<Self> {
        Self::resolve_in(path, device, source, check_only, game_stopped, None)
    }

    /// Resolves deployment inputs using an optional shared directory override.
    pub fn resolve_in(
        path: &Path,
        device: &str,
        source: Option<&Path>,
        check_only: bool,
        game_stopped: bool,
        shared_root_override: Option<&Path>,
    ) -> Result<Self> {
        if !check_only && !game_stopped {
            return Err(at(
                "arguments",
                "game_stop_required",
                invalid("Stop the game and pass --game-stopped"),
            ));
        }
        let path = project::location(path).map_err(|e| at("project", "project_error", e))?;
        let saved = project::load(&path).map_err(|e| at("project", "project_error", e))?;
        project::validate_device_name(device).map_err(|e| at("device", "invalid_device", e))?;
        // Schema v2 keeps device registrations outside the project file.
        let shared_root = if saved.schema_version == 2 {
            Some(match shared_root_override {
                Some(root) => root.to_path_buf(),
                None => shared::root().map_err(|e| at("device", "device_registry_error", e))?,
            })
        } else {
            None
        };
        let shared_devices = if let Some(root) = &shared_root {
            shared::load(root)
                .map_err(|e| at("device", "device_registry_error", e))?
                .0
                .devices
        } else {
            Default::default()
        };
        let devices = if saved.schema_version == 2 {
            &shared_devices
        } else {
            &saved.devices
        };
        let selected = devices.get(device).ok_or_else(|| {
            at(
                "device",
                "unknown_device",
                invalid("Device is not registered"),
            )
        })?;
        let parent = path.parent().unwrap();
        let source = source
            .map(Path::to_path_buf)
            .unwrap_or_else(|| parent.join(&saved.game.source));
        if source.as_os_str().is_empty() {
            return Err(at("build", "invalid_build", invalid("Empty source path")));
        }
        let build = Build::new(
            saved.game.title,
            source,
            saved.game.executable,
            saved.game.arguments,
            saved.game.runtime,
        )
        .map_err(|e| at("build", "invalid_build", e))?;
        Ok(Self {
            device: device.into(),
            host: selected.host.clone(),
            login: selected.login.clone(),
            auth_dir: if let Some(root) = shared_root {
                root.join("devkit-client-rust")
            } else {
                parent.join(saved.auth_dir)
            },
            build,
            check_only,
        })
    }

    /// Checks readiness or uploads the build through the given command runner.
    pub fn execute(&self, runner: &dyn Runner) -> Result<Value> {
        let auth = (|| {
            crate::regular(&self.auth_dir.join("devkit_rsa"))?;
            crate::regular(&self.auth_dir.join("known_hosts"))?;
            client::config(&self.auth_dir)
        })()
        .map_err(|e| at("auth", "auth_error", e))?;
        let _lock =
            lock(&auth.join("operation.lock")).map_err(|e| at("auth", "operation_locked", e))?;
        let connection = Connection {
            config: &auth,
            host: &self.host,
            login: &self.login,
            runner,
        };
        eprintln!(
            "Device: {} ({}@{}); title: {}; source: {}",
            self.device,
            self.login,
            self.host,
            self.build.title,
            self.build.source.display()
        );
        let mut result = if self.check_only {
            connection.ready_for(&self.build)?;
            json!({"checked": true})
        } else {
            connection.upload(&self.build)?
        };
        result["device"] = self.device.clone().into();
        result["host"] = self.host.clone().into();
        result["title"] = self.build.title.clone().into();
        result["source"] = self.build.source.to_string_lossy().as_ref().into();
        Ok(result)
    }
}
