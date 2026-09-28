//! Local tools for preparing and deploying game builds to SteamOS devices.

/// Manages client credentials, trusted hosts, and device connections.
pub mod client;
/// Resolves and executes project deployments.
pub mod deploy;
/// Registers devices through an interactive authentication workflow.
pub mod device;
/// Formats staged operation failures for CLI responses.
pub mod failure;
/// Reads, validates, and migrates project settings.
pub mod project;
/// Builds scripts and decodes responses for the remote protocol.
pub mod remote;
/// Manages the shared device registry and client directory.
pub mod shared;

use serde::{Deserialize, Serialize};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs, io,
    net::Ipv4Addr,
    path::{Component, Path, PathBuf},
    process::Command,
};

/// Result type used by local Devkit operations.
pub type Result<T> = io::Result<T>;
/// Creates an I/O error for invalid input or application state.
pub fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

/// Rejects title IDs outside the allowed ASCII format or reserved names.
pub fn validate_title(title: &str) -> Result<()> {
    let bytes = title.as_bytes();
    if !(2..=64).contains(&bytes.len())
        || !(bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        || !bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
        || ["steam", "steamvr", "steamdeckard", "steamvrdeckard"]
            .contains(&title.to_ascii_lowercase().as_str())
    {
        return Err(invalid("Invalid or reserved title ID"));
    }
    Ok(())
}

/// Parses a private or link-local IPv4 destination.
pub fn validate_host(host: &str) -> Result<Ipv4Addr> {
    let ip: Ipv4Addr = host
        .parse()
        .map_err(|_| invalid("Expected a private LAN IPv4 address"))?;
    if !ip.is_private() && !ip.is_link_local() {
        return Err(invalid("Expected a private LAN IPv4 address"));
    }
    Ok(ip)
}

/// Rejects login names that cannot be safely passed to SSH.
pub fn validate_login(login: &str) -> Result<()> {
    let bytes = login.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 32
        || !(bytes[0].is_ascii_lowercase() || bytes[0] == b'_')
        || !bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(b))
    {
        return Err(invalid("Invalid Linux login name"));
    }
    Ok(())
}

/// Quotes a string as one literal POSIX shell argument.
pub fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

/// Returns a UTF-8 path without control characters.
pub fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .filter(|s| !s.chars().any(char::is_control))
        .ok_or_else(|| invalid("Expected a UTF-8 path without control characters"))
}

/// Requires an existing regular file and rejects symbolic links.
pub fn regular(path: &Path) -> Result<()> {
    if !fs::symlink_metadata(path)?.is_file() {
        return Err(invalid("Expected a regular file, not a link"));
    }
    Ok(())
}

/// Creates or verifies a directory accessible only to its owner.
pub fn private_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
        return Err(invalid("Expected an owner-only directory (0700)"));
    }
    Ok(())
}

/// Replaces a regular file atomically after syncing the new contents.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    if fs::symlink_metadata(path).is_ok() {
        regular(path)?;
    }
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().ok_or_else(|| invalid("Missing parent"))?)?;
    file.write_all(data)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// Holds an exclusive file lock until it is released.
pub struct LockGuard {
    file: fs::File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

/// Acquires an exclusive nonblocking lock on an owner-only file.
pub fn lock(path: &Path) -> Result<LockGuard> {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    fs2::FileExt::try_lock_exclusive(&file).map_err(|_| invalid("Another operation is active"))?;
    Ok(LockGuard { file })
}

#[cfg(test)]
mod tests {
    use super::lock;

    #[test]
    fn lock_is_available_after_the_previous_guard_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("operation.lock");

        for _ in 0..10 {
            let guard = lock(&path).unwrap();
            drop(guard);
            lock(&path).unwrap();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Game build and launch settings used by validation and deployment.
pub struct Build {
    /// Steam title identifier used for installation.
    pub title: String,
    /// Local build directory or Android APK path.
    pub source: PathBuf,
    /// Executable name followed by its launch arguments.
    pub argv: Vec<String>,
    /// Runtime selection: `default`, `sniper`, or `android`.
    pub runtime: String,
}

impl Build {
    /// Resolves the source and validates the build and launch settings.
    pub fn new(
        title: String,
        source: PathBuf,
        executable: String,
        arguments: Vec<String>,
        runtime: String,
    ) -> Result<Self> {
        let source = source.canonicalize()?;
        let build = Self {
            title,
            source,
            argv: std::iter::once(executable).chain(arguments).collect(),
            runtime,
        };
        build.validate_metadata()?;
        build.validate_source()?;
        Ok(build)
    }

    /// Checks the source directory or APK and its executable content.
    pub fn validate_source(&self) -> Result<()> {
        self.validate_metadata()?;
        path_text(&self.source)?;
        if self.runtime == "android" {
            regular(&self.source)?;
            let verified = Command::new("/usr/bin/unzip")
                .args(["-tqq", "--"])
                .arg(&self.source)
                .output()?;
            if !verified.status.success() {
                return Err(invalid("Invalid APK archive"));
            }
            let listed = Command::new("/usr/bin/unzip")
                .args(["-Z", "-1", "--"])
                .arg(&self.source)
                .output()?;
            if !listed.status.success() {
                return Err(invalid("Cannot inspect APK archive"));
            }
            let entries =
                String::from_utf8(listed.stdout).map_err(|_| invalid("Invalid APK entry name"))?;
            if !entries.lines().any(|entry| entry == "AndroidManifest.xml") {
                return Err(invalid("APK is missing AndroidManifest.xml"));
            }
            // APKs without native libraries can run as-is; native APKs need ARM64.
            let native = entries.lines().any(|entry| entry.starts_with("lib/"));
            if native
                && !entries
                    .lines()
                    .any(|entry| entry.starts_with("lib/arm64-v8a/") && entry.ends_with(".so"))
            {
                return Err(invalid("APK is missing ARM64 native libraries"));
            }
        } else {
            if !self.source.is_dir() || self.source.parent().is_none() {
                return Err(invalid("Expected a build directory"));
            }
            validate_tree(&self.source)?;
            yuge_hasi_core::launch::inspect_elf(&mut fs::File::open(
                self.source.join(&self.argv[0]),
            )?)?;
        }
        Ok(())
    }

    /// Returns the CPU architecture required by this build's runtime.
    pub fn expected_architecture(&self) -> &'static str {
        if self.runtime == "android" {
            "aarch64"
        } else {
            "x86_64"
        }
    }

    /// Checks title, runtime, executable path, and launch arguments.
    pub fn validate_metadata(&self) -> Result<()> {
        validate_title(&self.title)?;
        if self.argv.is_empty()
            || self.argv.len() > 128
            || !["default", "sniper", "android"].contains(&self.runtime.as_str())
        {
            return Err(invalid("Invalid launch settings"));
        }
        for arg in &self.argv {
            if arg.len() > 8192 || arg.chars().any(char::is_control) {
                return Err(invalid("Invalid launch argument"));
            }
        }
        let path = Path::new(&self.argv[0]);
        // Android launches the archive itself, so a path or extra argument is ambiguous.
        if self.runtime == "android"
            && (self.argv.len() != 1
                || path.file_name().and_then(|name| name.to_str()) != Some(self.argv[0].as_str())
                || !self.argv[0].to_ascii_lowercase().ends_with(".apk")
                || self.source.file_name().and_then(|name| name.to_str())
                    != Some(self.argv[0].as_str()))
        {
            return Err(invalid(
                "Android start command must be the APK filename without arguments",
            ));
        }
        if self.argv[0].is_empty()
            || !path
                .components()
                .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
            || !path.components().any(|c| matches!(c, Component::Normal(_)))
        {
            return Err(invalid("Executable must stay within the build"));
        }
        Ok(())
    }
}

/// Rejects links, special files, invalid paths, and oversized build trees.
pub fn validate_tree(root: &Path) -> Result<()> {
    // File transfer must not follow links or device nodes outside the selected build.
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0usize;
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            count += 1;
            if count > 100_000 {
                return Err(invalid("Build contains too many entries"));
            }
            path_text(&entry.path())?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if !kind.is_file() {
                return Err(invalid("Links and special files are not supported"));
            }
        }
    }
    Ok(())
}
