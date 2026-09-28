use super::process::execute;
use crate::{Result, atomic_write, invalid, private_dir, regular, validate_host};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

// Credential files whose type and owner-only permissions are checked by `config`.
const IDENTITY_FILES: [&str; 3] = ["devkit_rsa", "devkit_rsa.pub", "known_hosts"];

// SSH public-key algorithms accepted when parsing client and host keys.
const SSH_KEY_TYPES: [&str; 5] = [
    "ssh-rsa",
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
];

/// Validates and returns the private directory that holds client credentials.
pub fn config(path: &Path) -> Result<PathBuf> {
    private_dir(path)?;
    for name in IDENTITY_FILES {
        validate_private_file(&path.join(name))?;
    }
    path.canonicalize()
}

// Accepts an absent credential file, but rejects links and shared permissions.
fn validate_private_file(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_err() {
        return Ok(());
    }
    regular(path)?;
    if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
        return Err(invalid("Config files must be owner-only"));
    }
    Ok(())
}

/// Creates the SSH client identity when absent and verifies an existing key pair.
pub fn init(config: &Path) -> Result<Value> {
    let key = config.join("devkit_rsa");
    let public = config.join("devkit_rsa.pub");
    if key.exists() != public.exists() {
        return Err(invalid(
            "Incomplete identity; recover it before initializing",
        ));
    }
    if !key.exists() {
        generate_identity(config, &key, &public)?;
    }
    verify_key_pair(&key, &public)?;
    Ok(json!({"initialized":true,"config":config}))
}

// Generates a key pair in a temporary directory before installing both files.
fn generate_identity(config: &Path, key: &Path, public: &Path) -> Result<()> {
    let temp = tempfile::tempdir_in(config)?;
    let generated = temp.path().join("key");
    execute(
        Command::new("/usr/bin/ssh-keygen")
            .args([
                "-q",
                "-t",
                "rsa",
                "-b",
                "3072",
                "-N",
                "",
                "-C",
                "yuge-hasi-devkit",
                "-f",
            ])
            .arg(&generated),
        b"",
    )?;
    atomic_write(key, &fs::read(&generated)?)?;
    atomic_write(public, &fs::read(generated.with_extension("pub"))?)
}

// Derives the public key from the private key to detect mismatched identity files.
fn verify_key_pair(key: &Path, public: &Path) -> Result<()> {
    let derived = execute(
        Command::new("/usr/bin/ssh-keygen")
            .args(["-y", "-P", "", "-f"])
            .arg(key),
        b"",
    )?;
    let derived =
        public_key(std::str::from_utf8(&derived).map_err(|_| invalid("Invalid public key"))?)?;
    if derived != public_key(&fs::read_to_string(public)?)? {
        return Err(invalid("Client key pair does not match"));
    }
    Ok(())
}

/// Parses a single supported SSH public key, excluding its optional comment.
pub fn public_key(text: &str) -> Result<String> {
    if text.trim().lines().count() != 1 {
        return Err(invalid("Expected one SSH public key"));
    }
    let fields: Vec<_> = text.split_whitespace().collect();
    let Some((&key_type, encoded)) = fields.first().zip(fields.get(1)) else {
        return Err(invalid("Unsupported SSH public key"));
    };
    if !SSH_KEY_TYPES.contains(&key_type) {
        return Err(invalid("Unsupported SSH public key"));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| invalid("Invalid key encoding"))?;
    let Some(length) = bytes
        .get(..4)
        .and_then(|prefix| prefix.try_into().ok())
        .map(u32::from_be_bytes)
    else {
        return Err(invalid("Invalid key blob"));
    };
    // SSH key blobs start with a length-prefixed algorithm name, which must match the label.
    let key_type_end = 4usize
        .checked_add(length as usize)
        .filter(|&end| end <= bytes.len())
        .ok_or_else(|| invalid("Key type mismatch"))?;
    if bytes[4..key_type_end] != *key_type.as_bytes() {
        return Err(invalid("Key type mismatch"));
    }
    Ok(format!("{key_type} {encoded}"))
}

/// Returns the OpenSSH SHA-256 fingerprint of a validated public key.
pub fn fingerprint(key: &str) -> Result<String> {
    let key = public_key(key)?;
    let encoded = key
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| invalid("Invalid key"))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| invalid("Invalid key"))?;
    Ok(format!(
        "SHA256:{}",
        STANDARD.encode(Sha256::digest(bytes)).trim_end_matches('=')
    ))
}

/// Pins a host key after confirming the fingerprint supplied by the operator.
pub fn trust(config: &Path, host: &str, key_file: &Path, expected: &str) -> Result<Value> {
    validate_host(host)?;
    let key = public_key(&fs::read_to_string(key_file)?)?;
    let actual = fingerprint(&key)?;
    if actual != expected {
        return Err(invalid(
            "Fingerprint does not match the value verified on the device",
        ));
    }
    verify_fingerprint_format(config, &key)?;
    pin_host_key(config, host, &key)?;
    Ok(json!({"host":host,"fingerprint":actual,"trusted":true}))
}

// Requires OpenSSH to accept the host key before it is written to known_hosts.
fn verify_fingerprint_format(config: &Path, key: &str) -> Result<()> {
    let temp = tempfile::NamedTempFile::new_in(config)?;
    fs::write(temp.path(), format!("{key}\n"))?;
    execute(
        Command::new("/usr/bin/ssh-keygen")
            .args(["-l", "-E", "sha256", "-f"])
            .arg(temp.path()),
        b"",
    )?;
    Ok(())
}

// Adds one host pin without replacing or hiding an existing conflicting key.
fn pin_host_key(config: &Path, host: &str, key: &str) -> Result<()> {
    let path = config.join("known_hosts");
    let mut saved = match fs::read_to_string(&path) {
        Ok(saved) => saved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let mut already_pinned = false;
    // Inspect every matching row: a valid row must not hide a conflicting duplicate.
    for line in saved.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.first() != Some(&host) {
            continue;
        }
        if fields.len() != 3 || format!("{} {}", fields[1], fields[2]) != key {
            return Err(invalid("A different host key is already pinned"));
        }
        already_pinned = true;
    }
    if already_pinned {
        return Ok(());
    }
    if !saved.is_empty() && !saved.ends_with('\n') {
        saved.push('\n');
    }
    saved.push_str(&format!("{host} {key}\n"));
    atomic_write(&path, saved.as_bytes())
}

/// Requests device-side pairing using this client's verified public key.
pub fn pair(config: &Path, host: &str) -> Result<Value> {
    validate_host(host)?;
    init(config)?;
    let key = public_key(&fs::read_to_string(config.join("devkit_rsa.pub"))?)?;
    let body = format!("{key} 900b919520e4cf601998a71eec318fec\n");
    let output = execute(
        Command::new("/usr/bin/curl")
            .args([
                "--silent",
                "--show-error",
                "--fail",
                "--noproxy",
                "*",
                "--connect-timeout",
                "5",
                "--max-time",
                "35",
                "--max-filesize",
                "65536",
                "--header",
                "Content-Type: text/plain",
                "--data-binary",
                "@-",
            ])
            .arg(format!("http://{host}:32000/register")),
        body.as_bytes(),
    )?;
    if device_rejected_pairing(&output) {
        return Err(invalid("Device rejected pairing"));
    }
    Ok(json!({"host":host,"pairing_request_completed":true,"ssh_verified":false}))
}

// Treats only an explicit JSON error or `success: false` as a rejection.
fn device_rejected_pairing(output: &[u8]) -> bool {
    // A JSON success body is not required; only an explicit rejection fails pairing.
    let Ok(value) = serde_json::from_slice::<Value>(output) else {
        return false;
    };
    value
        .get("error")
        .is_some_and(|error| !error.is_null() && error != false && error != "")
        || value.get("success") == Some(&json!(false))
}

#[cfg(test)]
mod tests {
    use super::device_rejected_pairing;

    // Pairing fails only when JSON explicitly reports an error or false success.
    #[test]
    fn pairing_rejection_requires_an_explicit_error_signal() {
        for output in [
            br#"{}"#.as_slice(),
            br#"{"success": true}"#.as_slice(),
            br#"not json"#.as_slice(),
            br#"{"error": null}"#.as_slice(),
            br#"{"error": false}"#.as_slice(),
            br#"{"error": ""}"#.as_slice(),
        ] {
            assert!(!device_rejected_pairing(output));
        }
        for output in [
            br#"{"success": false}"#.as_slice(),
            br#"{"error": "denied"}"#.as_slice(),
        ] {
            assert!(device_rejected_pairing(output));
        }
    }
}
