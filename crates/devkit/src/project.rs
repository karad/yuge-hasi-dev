use crate::{
    Build, Result, client, invalid, lock, path_text, shared, validate_host, validate_login,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Project settings for one game and, in schema v1, its local devices.
pub struct Project {
    /// Project format version (`1` for local settings, `2` for shared devices).
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "empty_path")]
    /// Legacy client credentials path; empty in schema v2.
    pub auth_dir: PathBuf,
    /// Game build and launch settings.
    pub game: Game,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    /// Legacy project-local devices; empty in schema v2.
    pub devices: BTreeMap<String, Device>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Game settings stored in a project file.
pub struct Game {
    /// Steam title identifier.
    pub title: String,
    /// Build location relative to the project or an absolute path.
    pub source: PathBuf,
    /// Executable path within the build or Android APK filename.
    pub executable: String,
    /// Launch arguments following the executable.
    pub arguments: Vec<String>,
    /// Runtime selected for this game.
    pub runtime: String,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Connection settings for a registered device.
pub struct Device {
    /// Private IPv4 address of the device.
    pub host: String,
    /// SSH login on the device.
    pub login: String,
}

/// Validates a lowercase device name used in a registry.
pub fn validate_device_name(name: &str) -> Result<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 32
        || !bytes[0].is_ascii_lowercase()
        || !bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(b))
    {
        return Err(invalid("Invalid device name"));
    }
    Ok(())
}

// Rejects empty or non-text paths before they enter project settings.
fn configured_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(invalid("Empty configured path"));
    }
    path_text(path)?;
    Ok(())
}

#[allow(clippy::ptr_arg)] // Serde's skip predicate receives the field's exact type.
// Keeps the legacy auth path out of schema v2 TOML when it is empty.
fn empty_path(path: &PathBuf) -> bool {
    path.as_os_str().is_empty()
}

impl Project {
    /// Validates the schema and all project settings.
    pub fn validate(&self) -> Result<()> {
        if ![1, 2].contains(&self.schema_version) {
            return Err(invalid("Unsupported project schema version"));
        }
        if self.schema_version == 1 {
            configured_path(&self.auth_dir)?;
        } else if !self.auth_dir.as_os_str().is_empty() || !self.devices.is_empty() {
            return Err(invalid(
                "Shared project settings cannot contain auth_dir or devices",
            ));
        }
        configured_path(&self.game.source)?;
        Build {
            title: self.game.title.clone(),
            source: self.game.source.clone(),
            argv: std::iter::once(self.game.executable.clone())
                .chain(self.game.arguments.clone())
                .collect(),
            runtime: self.game.runtime.clone(),
        }
        .validate_metadata()?;
        for (name, device) in &self.devices {
            validate_device_name(name)?;
            validate_host(&device.host)?;
            validate_login(&device.login)?;
        }
        Ok(())
    }

    /// Parses and validates a project TOML document.
    pub fn parse(text: &str) -> Result<Self> {
        let project: Self =
            toml::from_str(text).map_err(|e| invalid(format!("Invalid project TOML: {e}")))?;
        project.validate()?;
        Ok(project)
    }
}

/// Resolves a project path without following a final-component symbolic link.
pub fn location(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let parent = absolute
        .parent()
        .ok_or_else(|| invalid("Missing project parent"))?
        .canonicalize()?;
    let name = absolute
        .file_name()
        .ok_or_else(|| invalid("Missing project filename"))?;
    Ok(parent.join(name))
}

/// Loads and validates an owner-only project file.
pub fn load(path: &Path) -> Result<Project> {
    Ok(load_snapshot(path)?.0)
}

/// Loads a project together with its original bytes for conflict detection.
pub fn load_snapshot(path: &Path) -> Result<(Project, Vec<u8>)> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.permissions().mode() & 0o077 != 0 {
        return Err(invalid(
            "Expected an owner-only regular project file (0600)",
        ));
    }
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok((Project::parse(&text)?, text.into_bytes()))
}

/// Creates a schema v2 project file without overwriting an existing file.
pub fn initialize(path: &Path, build: Build) -> Result<PathBuf> {
    build.validate_metadata()?;
    let path = location(path)?;
    let parent = path.parent().unwrap();
    let _lock = operation_lock(&path)?;
    if fs::symlink_metadata(&path).is_ok() {
        return Err(invalid("Project file already exists"));
    }
    let build = Build::new(
        build.title,
        build.source,
        build.argv[0].clone(),
        build.argv[1..].to_vec(),
        build.runtime,
    )?;
    let source = build
        .source
        .strip_prefix(parent)
        .unwrap_or(&build.source)
        .to_path_buf();
    let project = Project {
        schema_version: 2,
        auth_dir: PathBuf::new(),
        game: Game {
            title: build.title,
            source,
            executable: build.argv[0].clone(),
            arguments: build.argv[1..].to_vec(),
            runtime: build.runtime,
        },
        devices: BTreeMap::new(),
    };
    project.validate()?;
    let content = toml::to_string_pretty(&project).map_err(|e| invalid(e.to_string()))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temp.write_all(content.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(&path).map_err(|e| e.error)?;
    fs::File::open(parent)?.sync_all()?;
    Ok(path)
}

/// Returns project settings with resolved build, auth, and device paths.
pub fn show(path: &Path) -> Result<serde_json::Value> {
    let path = location(path)?;
    let mut project = load(&path)?;
    let parent = path.parent().unwrap();
    project.game.source = parent.join(&project.game.source);
    if project.schema_version == 1 {
        project.auth_dir = parent.join(&project.auth_dir);
    } else {
        project.auth_dir = crate::shared::auth_dir()?;
        project.devices = crate::shared::load(&crate::shared::root()?)?.0.devices;
    }
    Ok(serde_json::json!({"project": path, "settings": project}))
}

/// Moves a schema v1 project's identity and devices into shared settings.
pub fn migrate(path: &Path) -> Result<serde_json::Value> {
    let path = location(path)?;
    let _project_lock = operation_lock(&path)?;
    let (mut project, original) = load_snapshot(&path)?;
    if project.schema_version != 1 {
        return Err(invalid("Project is already using shared settings"));
    }
    let root = shared::root()?;
    let _registry_lock = shared::operation_lock(&root)?;
    let (mut registry, registry_original) = shared::load(&root)?;
    // Detect name collisions before touching the shared credentials or device registry.
    for (name, device) in &project.devices {
        if registry
            .devices
            .get(name)
            .is_some_and(|existing| existing != device)
        {
            return Err(invalid(format!(
                "Device name conflicts with shared settings: {name}"
            )));
        }
    }

    let source_auth = path.parent().unwrap().join(&project.auth_dir);
    crate::regular(&source_auth.join("devkit_rsa"))?;
    crate::regular(&source_auth.join("devkit_rsa.pub"))?;
    let source_auth = client::config(&source_auth)?;
    client::init(&source_auth)?;
    let target_auth = client::config(&root.join("devkit-client-rust"))?;
    let _auth_lock = lock(&target_auth.join("operation.lock"))?;
    let source_key = fs::read(source_auth.join("devkit_rsa"))?;
    let source_public = fs::read(source_auth.join("devkit_rsa.pub"))?;
    let target_key = target_auth.join("devkit_rsa");
    let target_public = target_auth.join("devkit_rsa.pub");
    if target_key.exists() != target_public.exists() {
        return Err(invalid(
            "Incomplete shared identity; recover it before migration",
        ));
    }
    if target_key.exists() {
        // A shared identity cannot be silently replaced: it may already be trusted by devices.
        client::init(&target_auth)?;
        if fs::read(&target_key)? != source_key || fs::read(&target_public)? != source_public {
            return Err(invalid("Shared identity differs from the project identity"));
        }
    }
    let source_hosts = read_hosts(&source_auth.join("known_hosts"))?;
    let mut target_hosts = read_hosts(&target_auth.join("known_hosts"))?;
    // Keep every existing host pin, and reject any address whose key changed.
    for (host, key) in source_hosts {
        if target_hosts
            .get(&host)
            .is_some_and(|existing| existing != &key)
        {
            return Err(invalid(format!(
                "Host key conflicts with shared settings: {host}"
            )));
        }
        target_hosts.insert(host, key);
    }
    for device in project.devices.values() {
        // A migrated device must retain a trusted key before it becomes discoverable.
        if !target_hosts.contains_key(&device.host) {
            return Err(invalid(format!(
                "Missing trusted host key for {}",
                device.host
            )));
        }
    }
    if !target_key.exists() {
        crate::atomic_write(&target_key, &source_key)?;
        crate::atomic_write(&target_public, &source_public)?;
    }
    let hosts = target_hosts
        .into_iter()
        .map(|(host, key)| format!("{host} {key}\n"))
        .collect::<String>();
    crate::atomic_write(&target_auth.join("known_hosts"), hosts.as_bytes())?;
    registry
        .devices
        .extend(std::mem::take(&mut project.devices));
    shared::save(&root, &registry, registry_original.as_deref())?;
    // Switch the project schema last, after shared credentials and devices are usable.
    project.schema_version = 2;
    project.auth_dir = PathBuf::new();
    save_existing(&path, &project, &original)?;
    Ok(
        serde_json::json!({"migrated": true, "project": path, "auth_dir": target_auth, "devices": root.join("devices.toml")}),
    )
}

// Parses pinned hosts into a map while rejecting malformed or duplicate entries.
fn read_hosts(path: &Path) -> Result<BTreeMap<String, String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error),
    };
    let mut hosts = BTreeMap::new();
    // Duplicate entries make the pinned key for one host ambiguous.
    for line in text.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 3 {
            return Err(invalid("Invalid known_hosts entry"));
        }
        validate_host(fields[0])?;
        let key = client::public_key(&format!("{} {}", fields[1], fields[2]))?;
        if hosts.insert(fields[0].to_string(), key).is_some() {
            return Err(invalid("Duplicate known_hosts entry"));
        }
    }
    Ok(hosts)
}

/// Acquires the lock used when changing a project file.
pub fn operation_lock(path: &Path) -> Result<fs::File> {
    let name = path
        .file_name()
        .ok_or_else(|| invalid("Missing project filename"))?;
    lock(&path.with_file_name(format!("{}.lock", name.to_string_lossy())))
}

// The caller holds the project lock; external edits must not be overwritten.
/// Saves a project only if its on-disk bytes still match `original`.
pub fn save_existing(path: &Path, project: &Project, original: &[u8]) -> Result<()> {
    project.validate()?;
    load(path)?;
    if fs::read(path)? != original {
        return Err(invalid("Project changed during device registration"));
    }
    let content = toml::to_string_pretty(project).map_err(|e| invalid(e.to_string()))?;
    crate::atomic_write(path, content.as_bytes())?;
    fs::File::open(
        path.parent()
            .ok_or_else(|| invalid("Missing project parent"))?,
    )?
    .sync_all()
}
