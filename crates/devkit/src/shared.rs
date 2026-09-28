use crate::{
    Result, invalid, lock, private_dir,
    project::{self, Device},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fs,
    io::{ErrorKind, Read},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Device registrations shared across projects.
pub struct Registry {
    /// Registered devices indexed by their local names.
    pub devices: BTreeMap<String, Device>,
}

/// Returns the shared settings path beneath the user's home directory.
pub fn root() -> Result<PathBuf> {
    let home = env::var_os("HOME").ok_or_else(|| invalid("HOME is not set"))?;
    let home = PathBuf::from(home);
    if !home.is_absolute() || !home.is_dir() {
        return Err(invalid("HOME must be an existing absolute directory"));
    }
    Ok(home.join(".yuge-hasi"))
}

/// Returns the shared directory for client SSH credentials.
pub fn auth_dir() -> Result<PathBuf> {
    Ok(root()?.join("devkit-client-rust"))
}

/// Creates or validates the owner-only shared settings directory.
pub fn prepare(root: &Path) -> Result<()> {
    private_dir(root)
}

/// Locks the shared device registry for a registration update.
pub fn operation_lock(root: &Path) -> Result<fs::File> {
    prepare(root)?;
    lock(&root.join("devices.toml.lock"))
}

/// Loads the registry and its original bytes for conflict detection.
pub fn load(root: &Path) -> Result<(Registry, Option<Vec<u8>>)> {
    if fs::symlink_metadata(root).is_ok() {
        let meta = fs::symlink_metadata(root)?;
        if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
            return Err(invalid("Expected an owner-only directory (0700)"));
        }
    }
    let path = root.join("devices.toml");
    // O_NOFOLLOW rejects a linked registry; O_NONBLOCK avoids hanging on a FIFO.
    let mut file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok((Registry::default(), None));
        }
        Err(error) => return Err(error),
    };
    let meta = file.metadata()?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        return Err(invalid(
            "Expected an owner-only regular device registry (0600)",
        ));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text =
        std::str::from_utf8(&bytes).map_err(|_| invalid("Invalid device registry encoding"))?;
    let registry: Registry =
        toml::from_str(text).map_err(|e| invalid(format!("Invalid device registry TOML: {e}")))?;
    validate(&registry)?;
    Ok((registry, Some(bytes)))
}

/// Validates all registered device names and connection settings.
pub fn validate(registry: &Registry) -> Result<()> {
    for (name, device) in &registry.devices {
        project::validate_device_name(name)?;
        crate::validate_host(&device.host)?;
        crate::validate_login(&device.login)?;
    }
    Ok(())
}

/// Saves a registry only if its on-disk contents match `original`.
pub fn save(root: &Path, registry: &Registry, original: Option<&[u8]>) -> Result<()> {
    validate(registry)?;
    let current = load(root)?.1;
    if current.as_deref() != original {
        return Err(invalid("Device registry changed during registration"));
    }
    let content = toml::to_string_pretty(registry).map_err(|e| invalid(e.to_string()))?;
    crate::atomic_write(&root.join("devices.toml"), content.as_bytes())?;
    fs::File::open(root)?.sync_all()
}
