use crate::{
    Result, client,
    failure::at,
    invalid, lock,
    project::{self, Device},
    shared,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{self, IsTerminal, Write},
    path::Path,
    process::{Command, Stdio},
};

/// Supplies terminal interaction for device registration.
pub trait Interaction {
    /// Reports whether prompts can be shown to the user.
    fn interactive(&self) -> bool;

    /// Displays a prompt and reads one response.
    fn read(&mut self, prompt: &str) -> Result<String>;
}

/// Standard-input and standard-error terminal interaction.
pub struct Terminal;

impl Interaction for Terminal {
    // Prompts require both input and error output to be attached to terminals.
    fn interactive(&self) -> bool {
        io::stdin().is_terminal() && io::stderr().is_terminal()
    }

    // Displays a prompt and treats end-of-input as cancellation.
    fn read(&mut self, prompt: &str) -> Result<String> {
        eprint!("{prompt}");
        io::stderr().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            return Err(invalid("Input canceled"));
        }
        Ok(line.trim().to_string())
    }
}

#[derive(Debug, PartialEq)]
/// Result of checking SSH public-key authentication.
pub enum Authentication {
    /// SSH authentication succeeded.
    Authenticated,
    /// The device explicitly refused the client's public key.
    Rejected,
}

/// Device operations required by the registration workflow.
pub trait Backend {
    /// Ensures the client identity exists and is valid.
    fn initialize(&self, auth: &Path) -> Result<()>;

    /// Reads the device's Ed25519 host key.
    fn scan(&self, host: &str) -> Result<String>;

    /// Pins a host key after verifying its fingerprint.
    fn trust(&self, auth: &Path, host: &str, key: &str, expected: &str) -> Result<()>;

    /// Checks whether the client can authenticate over SSH.
    fn authenticate(&self, auth: &Path, host: &str, login: &str) -> Result<Authentication>;

    /// Requests pairing with the device.
    fn pair(&self, auth: &Path, host: &str) -> Result<()>;
}

/// Backend that performs device operations through local system commands.
pub struct SystemBackend;

impl Backend for SystemBackend {
    // Ensures the client identity is present before scanning or connecting.
    fn initialize(&self, auth: &Path) -> Result<()> {
        client::init(auth).map(|_| ())
    }

    // Fetches the Ed25519 host key and rejects unexpected scan output.
    fn scan(&self, host: &str) -> Result<String> {
        crate::validate_host(host)?;
        let result = client::execute(
            Command::new("/usr/bin/ssh-keyscan").args(["-T", "5", "-t", "ed25519", host]),
            b"",
        )?;
        scanned_key(
            host,
            std::str::from_utf8(&result).map_err(|_| invalid("Invalid key scan encoding"))?,
        )
    }

    // Passes the scanned key and expected fingerprint to the client trust workflow.
    fn trust(&self, auth: &Path, host: &str, key: &str, expected: &str) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(auth)?;
        writeln!(file, "{key}")?;
        client::trust(auth, host, file.path(), expected).map(|_| ())
    }

    // Disables password methods so an SSH refusal can safely trigger pairing.
    fn authenticate(&self, auth: &Path, host: &str, login: &str) -> Result<Authentication> {
        let connection = client::Connection::new(auth, host, login);
        connection.check()?;
        let output = Command::new("/usr/bin/ssh")
            .env("LC_ALL", "C")
            .args(client::ssh_args(auth)?)
            .args([
                "-o",
                "PreferredAuthentications=publickey",
                "-o",
                "PasswordAuthentication=no",
                "-o",
                "KbdInteractiveAuthentication=no",
            ])
            .arg(format!("{login}@{host}"))
            .arg("true")
            .stdin(Stdio::null())
            .output()?;
        classify_authentication(output.status.code(), &output.stderr, host, login)
    }

    // Sends the client public key to the device's pairing endpoint.
    fn pair(&self, auth: &Path, host: &str) -> Result<()> {
        client::pair(auth, host).map(|_| ())
    }
}

/// Extracts one unambiguous Ed25519 host key from an SSH scan response.
pub fn scanned_key(host: &str, text: &str) -> Result<String> {
    crate::validate_host(host)?;
    let mut key = None;
    for line in text
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
    {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() != 3 || fields[0] != host || fields[1] != "ssh-ed25519" {
            return Err(invalid("Unexpected host key scan response"));
        }
        let found = client::public_key(&format!("{} {}", fields[1], fields[2]))?;
        if key.as_ref().is_some_and(|saved| saved != &found) {
            return Err(invalid("Conflicting host keys in scan"));
        }
        key = Some(found);
    }
    key.ok_or_else(|| invalid("No Ed25519 host key received; inspect the device SSH configuration"))
}

/// Distinguishes an explicit public-key refusal from other SSH failures.
pub fn classify_authentication(
    code: Option<i32>,
    stderr: &[u8],
    host: &str,
    login: &str,
) -> Result<Authentication> {
    if code == Some(0) {
        return Ok(Authentication::Authenticated);
    }
    let message = String::from_utf8_lossy(stderr);
    // Only an unambiguous authentication refusal permits a pairing request.
    let methods = message
        .trim()
        .strip_prefix(&format!("{login}@{host}: Permission denied ("))
        .and_then(|s| s.strip_suffix(")."));
    if code == Some(255)
        && methods.is_some_and(|s| {
            s.split(',').any(|m| m == "publickey")
                && s.split(',')
                    .all(|m| matches!(m, "publickey" | "password" | "keyboard-interactive"))
        })
    {
        return Ok(Authentication::Rejected);
    }
    Err(invalid(format!(
        "SSH verification failed: {}",
        message.trim()
    )))
}

// Reports whether a host already has a pinned key without creating known_hosts.
fn pinned(auth: &Path, host: &str) -> Result<bool> {
    match fs::read_to_string(auth.join("known_hosts")) {
        Ok(text) => Ok(text
            .lines()
            .any(|line| line.split_whitespace().next() == Some(host))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Registers a device using project or shared settings.
pub fn add(
    path: &Path,
    name: &str,
    host: &str,
    login: &str,
    interaction: &mut dyn Interaction,
    backend: &dyn Backend,
) -> Result<Value> {
    add_in(path, name, host, login, interaction, backend, None)
}

/// Returns the registered devices for a project.
pub fn list(path: &Path) -> Result<Value> {
    list_in(path, None)
}

/// Returns registered devices using an optional shared settings directory override.
pub fn list_in(path: &Path, shared_root_override: Option<&Path>) -> Result<Value> {
    let path = project::location(path).map_err(|e| at("project", "project_error", e))?;
    let _project_lock =
        project::operation_lock(&path).map_err(|e| at("project", "project_locked", e))?;
    let (saved, _) =
        project::load_snapshot(&path).map_err(|e| at("project", "project_error", e))?;
    let devices = if saved.schema_version == 2 {
        let root = shared_root_override
            .map(Path::to_path_buf)
            .map_or_else(|| shared::root(), Ok)
            .map_err(|e| at("auth", "auth_error", e))?;
        shared::load(&root)
            .map_err(|e| at("device", "device_registry_error", e))?
            .0
            .devices
    } else {
        saved.devices
    };
    let devices: Vec<_> = devices
        .into_iter()
        .map(|(name, device)| json!({"name": name, "host": device.host, "login": device.login}))
        .collect();
    Ok(json!({"devices": devices}))
}

/// Removes a locally registered device without changing device-side SSH authorization.
pub fn remove(path: &Path, name: &str) -> Result<Value> {
    remove_in(path, name, None)
}

/// Removes a locally registered device using an optional shared settings directory override.
pub fn remove_in(path: &Path, name: &str, shared_root_override: Option<&Path>) -> Result<Value> {
    project::validate_device_name(name).map_err(|e| at("device", "invalid_device", e))?;
    let path = project::location(path).map_err(|e| at("project", "project_error", e))?;
    let _project_lock =
        project::operation_lock(&path).map_err(|e| at("project", "project_locked", e))?;
    let (mut saved, original) =
        project::load_snapshot(&path).map_err(|e| at("project", "project_error", e))?;
    if saved.schema_version == 2 {
        let root = shared_root_override
            .map(Path::to_path_buf)
            .map_or_else(|| shared::root(), Ok)
            .map_err(|e| at("auth", "auth_error", e))?;
        let _registry_lock =
            shared::operation_lock(&root).map_err(|e| at("device", "device_locked", e))?;
        let (mut registry, registry_original) =
            shared::load(&root).map_err(|e| at("device", "device_registry_error", e))?;
        let device = registry.devices.remove(name).ok_or_else(|| {
            at(
                "device",
                "unknown_device",
                invalid("Device is not registered"),
            )
        })?;
        shared::save(&root, &registry, registry_original.as_deref())
            .map_err(|e| at("device", "device_save_failed", e))?;
        return Ok(
            json!({"removed": true, "device": name, "host": device.host, "login": device.login}),
        );
    }
    let device = saved.devices.remove(name).ok_or_else(|| {
        at(
            "device",
            "unknown_device",
            invalid("Device is not registered"),
        )
    })?;
    project::save_existing(&path, &saved, &original)
        .map_err(|e| at("project", "project_save_failed", e))?;
    Ok(json!({"removed": true, "device": name, "host": device.host, "login": device.login}))
}

/// Registers a device using an optional shared settings directory override.
pub fn add_in(
    path: &Path,
    name: &str,
    host: &str,
    login: &str,
    interaction: &mut dyn Interaction,
    backend: &dyn Backend,
    shared_root_override: Option<&Path>,
) -> Result<Value> {
    if !interaction.interactive() {
        return Err(at(
            "interaction",
            "terminal_required",
            invalid("Run device add in an interactive terminal"),
        ));
    }
    project::validate_device_name(name).map_err(|e| at("device", "invalid_device", e))?;
    crate::validate_host(host).map_err(|e| at("device", "invalid_device", e))?;
    crate::validate_login(login).map_err(|e| at("device", "invalid_device", e))?;
    let path = project::location(path).map_err(|e| at("project", "project_error", e))?;
    // Hold the project lock while selecting the schema and its device registry.
    let _project_lock =
        project::operation_lock(&path).map_err(|e| at("project", "project_locked", e))?;
    let (mut saved, original) =
        project::load_snapshot(&path).map_err(|e| at("project", "project_error", e))?;
    let shared_root = if saved.schema_version == 2 {
        Some(match shared_root_override {
            Some(root) => root.to_path_buf(),
            None => shared::root().map_err(|e| at("auth", "auth_error", e))?,
        })
    } else {
        None
    };
    let _registry_lock = if let Some(root) = &shared_root {
        Some(shared::operation_lock(root).map_err(|e| at("device", "device_locked", e))?)
    } else {
        None
    };
    let (mut registry, registry_original) = if let Some(root) = &shared_root {
        shared::load(root).map_err(|e| at("device", "device_registry_error", e))?
    } else {
        (shared::Registry::default(), None)
    };
    let devices = if shared_root.is_some() {
        &registry.devices
    } else {
        &saved.devices
    };
    let device = Device {
        host: host.into(),
        login: login.into(),
    };
    if devices
        .get(name)
        .is_some_and(|existing| existing != &device)
    {
        return Err(at(
            "device",
            "device_conflict",
            invalid("Device name already has different connection settings"),
        ));
    }
    let auth_path = if let Some(root) = &shared_root {
        root.join("devkit-client-rust")
    } else {
        path.parent().unwrap().join(&saved.auth_dir)
    };
    let auth = client::config(&auth_path).map_err(|e| at("auth", "auth_error", e))?;
    let _auth_lock =
        lock(&auth.join("operation.lock")).map_err(|e| at("auth", "operation_locked", e))?;
    backend
        .initialize(&auth)
        .map_err(|e| at("auth", "auth_error", e))?;
    // First use requires an out-of-band fingerprint check before SSH trusts this host.
    if !pinned(&auth, host).map_err(|e| at("trust", "host_key_error", e))? {
        let key = backend
            .scan(host)
            .map_err(|e| at("trust", "host_key_error", e))?;
        let fingerprint =
            client::fingerprint(&key).map_err(|e| at("trust", "host_key_error", e))?;
        let expected = interaction.read(&format!("Device: {name} ({login}@{host})\nScanned fingerprint: {fingerprint}\nOn the device, run whoami and ssh-keygen -lf /etc/ssh/ssh_host_ed25519_key.pub -E sha256. Verify the login is {login}.\nEnter the full SHA256 fingerprint shown on the device (empty to cancel): "))
            .map_err(|e| at("interaction", "input_canceled", e))?;
        if expected != fingerprint {
            return Err(at(
                "trust",
                "fingerprint_mismatch",
                invalid("Fingerprint does not match the value verified on the device"),
            ));
        }
        backend
            .trust(&auth, host, &key, &expected)
            .map_err(|e| at("trust", "host_key_error", e))?;
    }
    let mut paired = false;
    // Request pairing only after SSH explicitly rejects the client's public key.
    if backend
        .authenticate(&auth, host, login)
        .map_err(|e| at("connection", "connection_failed", e))?
        == Authentication::Rejected
    {
        let answer = interaction.read("On the device, enable Developer Mode and open Settings > Developer > Development Kit > Pair new host. Type pair to send the request: ")
            .map_err(|e| at("interaction", "input_canceled", e))?;
        if answer != "pair" {
            return Err(at(
                "interaction",
                "input_canceled",
                invalid("Pairing canceled"),
            ));
        }
        backend
            .pair(&auth, host)
            .map_err(|e| at("pairing", "pairing_failed", e))?;
        let answer = interaction
            .read("Approve this Mac on the device. Type approved after approval: ")
            .map_err(|e| at("interaction", "input_canceled", e))?;
        if answer != "approved" {
            return Err(at(
                "interaction",
                "input_canceled",
                invalid("Pairing approval not confirmed"),
            ));
        }
        if backend
            .authenticate(&auth, host, login)
            .map_err(|e| at("connection", "connection_failed", e))?
            != Authentication::Authenticated
        {
            return Err(at(
                "connection",
                "authentication_rejected",
                invalid("SSH authentication is still rejected; device settings were not saved"),
            ));
        }
        paired = true;
    }
    // Do not publish a device until authentication succeeds, including after pairing.
    let reused = devices.get(name) == Some(&device);
    if !reused {
        if let Some(root) = &shared_root {
            registry.devices.insert(name.into(), device);
            shared::save(root, &registry, registry_original.as_deref())
                .map_err(|e| at("device", "device_save_failed", e))?;
        } else {
            saved.devices.insert(name.into(), device);
            project::save_existing(&path, &saved, &original)
                .map_err(|e| at("project", "project_save_failed", e))?;
        }
    }
    Ok(
        json!({"device": name, "host": host, "login": login, "authenticated": true, "paired": paired, "reused": reused}),
    )
}
