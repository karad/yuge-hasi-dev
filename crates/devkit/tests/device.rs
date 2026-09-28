use base64::{Engine, engine::general_purpose::STANDARD};
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::{Command, Stdio},
};
use yuge_hasi_devkit::{
    Result, client,
    device::{self, Authentication, Backend, Interaction},
    failure, invalid, project, shared,
};

// Private LAN address used by device registration fixtures and assertions.
const HOST: &str = "192.168.1.2";

// Creates a stable Ed25519 key used in host scan responses.
fn key() -> String {
    let mut blob = 11u32.to_be_bytes().to_vec();
    blob.extend(b"ssh-ed25519");
    blob.extend(32u32.to_be_bytes());
    blob.extend([1u8; 32]);
    format!("ssh-ed25519 {}", STANDARD.encode(blob))
}

// Writes a legacy project with project-local authentication settings.
fn fixture(root: &Path) -> std::path::PathBuf {
    let path = root.join("yuge-hasi-devkit.toml");
    fs::write(&path, "schema_version = 1\nauth_dir = 'auth'\n[game]\ntitle = 'pocket_pop'\nsource = 'not-built-yet'\nexecutable = 'Game'\narguments = []\nruntime = 'default'\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

// Writes a schema v2 project that uses shared device settings.
fn shared_fixture(root: &Path) -> std::path::PathBuf {
    let path = root.join("project.toml");
    fs::write(&path, "schema_version = 2\n[game]\ntitle = 'pocket_pop'\nsource = 'not-built-yet'\nexecutable = 'Game'\narguments = []\nruntime = 'default'\n").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    path
}

struct Prompts {
    enabled: bool,
    answers: VecDeque<String>,
    count: usize,
}

impl Prompts {
    // Queues responses for an interactive registration attempt.
    fn new(answers: &[&str]) -> Self {
        Self {
            enabled: true,
            answers: answers.iter().map(|s| (*s).into()).collect(),
            count: 0,
        }
    }
}

impl Interaction for Prompts {
    // Allows tests to simulate terminal and nonterminal sessions.
    fn interactive(&self) -> bool {
        self.enabled
    }

    // Consumes one queued answer and records each prompt.
    fn read(&mut self, _: &str) -> Result<String> {
        self.count += 1;
        self.answers.pop_front().ok_or_else(|| invalid("Canceled"))
    }
}

struct Fake {
    calls: RefCell<Vec<&'static str>>,
    auth: RefCell<VecDeque<Result<Authentication>>>,
    pair_error: bool,
}

impl Fake {
    // Queues SSH authentication outcomes for the registration flow.
    fn new(auth: Vec<Result<Authentication>>) -> Self {
        Self {
            calls: Default::default(),
            auth: RefCell::new(auth.into()),
            pair_error: false,
        }
    }
}

impl Backend for Fake {
    // Records identity initialization without invoking SSH key generation.
    fn initialize(&self, _: &Path) -> Result<()> {
        self.calls.borrow_mut().push("init");
        Ok(())
    }

    // Supplies the fixed host key fixture.
    fn scan(&self, _: &str) -> Result<String> {
        self.calls.borrow_mut().push("scan");
        Ok(key())
    }

    // Records trust requests while exercising the real host-key validation.
    fn trust(&self, auth: &Path, host: &str, value: &str, expected: &str) -> Result<()> {
        self.calls.borrow_mut().push("trust");
        Backend::trust(&device::SystemBackend, auth, host, value, expected)
    }

    // Returns the next queued SSH authentication result.
    fn authenticate(&self, _: &Path, _: &str, _: &str) -> Result<Authentication> {
        self.calls.borrow_mut().push("authenticate");
        self.auth
            .borrow_mut()
            .pop_front()
            .expect("Unexpected authentication")
    }

    // Records pairing and optionally simulates its failure.
    fn pair(&self, _: &Path, _: &str) -> Result<()> {
        self.calls.borrow_mut().push("pair");
        if self.pair_error {
            Err(invalid("Pairing failed"))
        } else {
            Ok(())
        }
    }
}

// Registration pins the verified host, pairs, and saves only after SSH succeeds.
#[test]
fn first_registration_checks_fingerprint_pairs_then_saves_only_after_authentication() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let fingerprint = client::fingerprint(&key()).unwrap();
    let mut prompts = Prompts::new(&[&fingerprint, "pair", "approved"]);
    let fake = Fake::new(vec![
        Ok(Authentication::Rejected),
        Ok(Authentication::Authenticated),
    ]);
    let result = device::add(&path, "deck", HOST, "deck", &mut prompts, &fake).unwrap();
    assert_eq!(result["paired"], true);
    assert_eq!(result["authenticated"], true);
    assert_eq!(project::load(&path).unwrap().devices["deck"].host, HOST);
    assert_eq!(
        *fake.calls.borrow(),
        [
            "init",
            "scan",
            "trust",
            "authenticate",
            "pair",
            "authenticate"
        ]
    );
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_dir(dir.path().join("auth")).unwrap().count(), 2);
}

#[test]
fn list_and_remove_manage_legacy_project_devices_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let original = fs::read(&path).unwrap();
    let mut saved = project::load(&path).unwrap();
    saved.devices.insert(
        "deck".into(),
        project::Device {
            host: HOST.into(),
            login: "deck".into(),
        },
    );
    project::save_existing(&path, &saved, &original).unwrap();

    assert_eq!(
        device::list(&path).unwrap(),
        serde_json::json!({"devices": [{"name": "deck", "host": HOST, "login": "deck"}]})
    );
    assert_eq!(
        device::remove(&path, "deck").unwrap(),
        serde_json::json!({"removed": true, "device": "deck", "host": HOST, "login": "deck"})
    );
    assert_eq!(
        device::list(&path).unwrap(),
        serde_json::json!({"devices": []})
    );
    let error = device::remove(&path, "deck").unwrap_err();
    assert_eq!(failure::report(&error)["error"]["code"], "unknown_device");
}

#[test]
fn list_and_remove_manage_shared_devices_without_rewriting_the_project() {
    let dir = tempfile::tempdir().unwrap();
    let path = shared_fixture(dir.path());
    let root = dir.path().join("shared");
    shared::prepare(&root).unwrap();
    let (mut registry, original) = shared::load(&root).unwrap();
    registry.devices.insert(
        "deck".into(),
        project::Device {
            host: HOST.into(),
            login: "deck".into(),
        },
    );
    shared::save(&root, &registry, original.as_deref()).unwrap();
    let before = fs::read(&path).unwrap();

    assert_eq!(
        device::list_in(&path, Some(&root)).unwrap(),
        serde_json::json!({"devices": [{"name": "deck", "host": HOST, "login": "deck"}]})
    );
    device::remove_in(&path, "deck", Some(&root)).unwrap();
    assert_eq!(
        device::list_in(&path, Some(&root)).unwrap(),
        serde_json::json!({"devices": []})
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

// A trusted authenticated device is reused without prompts or file changes.
#[test]
fn trusted_authenticated_device_is_reused_without_prompts_or_rewriting_project() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let fingerprint = client::fingerprint(&key()).unwrap();
    let first = Fake::new(vec![Ok(Authentication::Authenticated)]);
    device::add(
        &path,
        "deck",
        HOST,
        "deck",
        &mut Prompts::new(&[&fingerprint]),
        &first,
    )
    .unwrap();
    let before = fs::read(&path).unwrap();
    let known = fs::read(dir.path().join("auth/known_hosts")).unwrap();
    let fake = Fake::new(vec![Ok(Authentication::Authenticated)]);
    let mut prompts = Prompts::new(&[]);
    let result = device::add(&path, "deck", HOST, "deck", &mut prompts, &fake).unwrap();
    assert_eq!(result["reused"], true);
    assert_eq!(result["paired"], false);
    assert_eq!(prompts.count, 0);
    assert_eq!(*fake.calls.borrow(), ["init", "authenticate"]);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(
        fs::read(dir.path().join("auth/known_hosts")).unwrap(),
        known
    );
}

// Fingerprint mismatch and cancellation stop before trust, pairing, or save.
#[test]
fn fingerprint_mismatch_or_cancel_never_trusts_pairs_or_saves() {
    for answer in ["SHA256:wrong", ""] {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path());
        let before = fs::read(&path).unwrap();
        let fake = Fake::new(vec![]);
        let error = device::add(
            &path,
            "deck",
            HOST,
            "deck",
            &mut Prompts::new(&[answer]),
            &fake,
        )
        .unwrap_err();
        assert_eq!(
            failure::report(&error)["error"]["code"],
            "fingerprint_mismatch"
        );
        assert_eq!(*fake.calls.borrow(), ["init", "scan"]);
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(!dir.path().join("auth/known_hosts").exists());
    }
}

// Later failures preserve project settings and already verified host trust.
#[test]
fn failures_keep_project_unchanged_and_verified_trust_for_retry() {
    for scenario in 0..5 {
        let dir = tempfile::tempdir().unwrap();
        let path = fixture(dir.path());
        let before = fs::read(&path).unwrap();
        let fingerprint = client::fingerprint(&key()).unwrap();
        let mut fake = Fake::new(if scenario == 0 {
            vec![Err(invalid("Connection timed out"))]
        } else {
            vec![Ok(Authentication::Rejected), Ok(Authentication::Rejected)]
        });
        fake.pair_error = scenario == 1;
        let answers = match scenario {
            3 => vec![fingerprint.as_str(), "cancel"],
            4 => vec![fingerprint.as_str(), "pair", "cancel"],
            _ => vec![fingerprint.as_str(), "pair", "approved"],
        };
        assert!(
            device::add(
                &path,
                "deck",
                HOST,
                "deck",
                &mut Prompts::new(&answers),
                &fake
            )
            .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(
            fs::read_to_string(dir.path().join("auth/known_hosts"))
                .unwrap()
                .contains(HOST)
        );
        if scenario == 0 {
            assert!(!fake.calls.borrow().contains(&"pair"));
        }
    }
}

// Input, conflict, and lock failures occur before authentication changes.
#[test]
fn nonterminal_conflict_and_project_lock_fail_before_auth_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let fake = Fake::new(vec![]);
    let mut prompts = Prompts::new(&[]);
    prompts.enabled = false;
    assert!(device::add(&path, "deck", HOST, "deck", &mut prompts, &fake).is_err());
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    prompts.enabled = true;
    let lock = project::operation_lock(&path).unwrap();
    assert!(device::add(&path, "deck", HOST, "deck", &mut prompts, &fake).is_err());
    fs2::FileExt::unlock(&lock).unwrap();
    drop(lock);
    let mut saved = project::load(&path).unwrap();
    saved.devices.insert(
        "deck".into(),
        project::Device {
            host: "192.168.1.3".into(),
            login: "deck".into(),
        },
    );
    let bytes = fs::read(&path).unwrap();
    project::save_existing(&path, &saved, &bytes).unwrap();
    assert!(device::add(&path, "deck", HOST, "deck", &mut prompts, &fake).is_err());
    assert!(fake.calls.borrow().is_empty());
    assert!(!dir.path().join("auth").exists());
}

// Only an explicit public-key refusal permits pairing.
#[test]
fn ssh_refusal_classification_is_conservative() {
    assert_eq!(
        device::classify_authentication(Some(0), b"", HOST, "deck").unwrap(),
        Authentication::Authenticated
    );
    let refusal = format!("deck@{HOST}: Permission denied (publickey).\r\n");
    assert_eq!(
        device::classify_authentication(Some(255), refusal.as_bytes(), HOST, "deck").unwrap(),
        Authentication::Rejected
    );
    for message in [
        "Connection timed out",
        "REMOTE HOST IDENTIFICATION HAS CHANGED!",
        "Permission denied",
        "deck@192.168.1.2: Permission denied (password).",
        "Warning: host key changed\ndeck@192.168.1.2: Permission denied (publickey).",
    ] {
        assert!(
            device::classify_authentication(Some(255), message.as_bytes(), HOST, "deck").is_err()
        );
    }
    assert!(device::classify_authentication(Some(1), refusal.as_bytes(), HOST, "deck").is_err());
}

// Host scanning rejects wrong addresses, key types, and conflicting keys.
#[test]
fn scan_requires_matching_host_and_unambiguous_ed25519_key() {
    let line = format!("{HOST} {}\n", key());
    assert_eq!(device::scanned_key(HOST, &line).unwrap(), key());
    assert_eq!(
        device::scanned_key(HOST, &(line.clone() + &line)).unwrap(),
        key()
    );
    for text in [
        "".into(),
        line.replace(HOST, "192.168.1.3"),
        "# comment only".into(),
        format!("{HOST} ssh-rsa invalid\n"),
        line.replace("ssh-ed25519", "ssh-rsa"),
    ] {
        assert!(device::scanned_key(HOST, &text).is_err());
    }
}

// Concurrent edits and unsafe files cannot be overwritten.
#[test]
fn save_rejects_external_changes_and_symlinks_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let original = fs::read(&path).unwrap();
    let saved = project::load(&path).unwrap();
    fs::write(&path, [original.clone(), b"\n# edit\n".to_vec()].concat()).unwrap();
    let changed = fs::read(&path).unwrap();
    assert!(project::save_existing(&path, &saved, &original).is_err());
    assert_eq!(fs::read(&path).unwrap(), changed);
    let link = dir.path().join("link");
    symlink(&path, &link).unwrap();
    assert!(project::save_existing(&link, &saved, &changed).is_err());
}

// A noninteractive CLI failure leaves device files untouched.
#[test]
fn cli_nonterminal_returns_json_without_any_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let before = fs::read(&path).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .current_dir(dir.path())
        .args([
            "devkit", "device", "add", "deck", "--host", HOST, "--login", "deck",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["error"]["code"], "terminal_required");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
}

// An explicit public-key refusal may list other SSH methods and still pair.
#[test]
fn explicit_auth_refusal_with_other_offered_methods_still_allows_pairing() {
    for methods in [
        "publickey,password",
        "publickey,keyboard-interactive",
        "publickey,password,keyboard-interactive",
    ] {
        let message = format!("deck@{HOST}: Permission denied ({methods}).");
        assert_eq!(
            device::classify_authentication(Some(255), message.as_bytes(), HOST, "deck").unwrap(),
            Authentication::Rejected
        );
    }
}

// Shared registration becomes available to other projects without rewriting them.
#[test]
fn shared_registration_is_available_across_projects_without_rewriting_them() {
    let dir = tempfile::tempdir().unwrap();
    let first = shared_fixture(dir.path());
    let second_dir = dir.path().join("second");
    fs::create_dir(&second_dir).unwrap();
    let second = shared_fixture(&second_dir);
    let root = dir.path().join("shared");
    let before = fs::read(&first).unwrap();
    let fingerprint = client::fingerprint(&key()).unwrap();
    let fake = Fake::new(vec![Ok(Authentication::Authenticated)]);
    device::add_in(
        &first,
        "deck",
        HOST,
        "deck",
        &mut Prompts::new(&[&fingerprint]),
        &fake,
        Some(&root),
    )
    .unwrap();
    assert_eq!(fs::read(&first).unwrap(), before);
    assert_eq!(
        yuge_hasi_devkit::shared::load(&root).unwrap().0.devices["deck"].host,
        HOST
    );
    assert!(!second_dir.join("auth").exists());
    assert!(!project::load(&second).unwrap().devices.contains_key("deck"));
    assert_eq!(
        fs::metadata(root.join("devices.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    let conflict = device::add_in(
        &second,
        "deck",
        "192.168.1.3",
        "deck",
        &mut Prompts::new(&[]),
        &Fake::new(vec![]),
        Some(&root),
    )
    .unwrap_err();
    assert_eq!(
        failure::report(&conflict)["error"]["code"],
        "device_conflict"
    );
}

// A new address can reuse identity; a held auth lock prevents work.
#[test]
fn new_address_can_reuse_identity_without_pairing_and_auth_lock_stops_work() {
    let dir = tempfile::tempdir().unwrap();
    let path = fixture(dir.path());
    let auth = client::config(&dir.path().join("auth")).unwrap();
    let guard = yuge_hasi_devkit::lock(&auth.join("operation.lock")).unwrap();
    let fake = Fake::new(vec![Ok(Authentication::Authenticated)]);
    let fingerprint = client::fingerprint(&key()).unwrap();
    assert!(
        device::add(
            &path,
            "deck_new_ip",
            "192.168.1.9",
            "deck",
            &mut Prompts::new(&[&fingerprint]),
            &fake
        )
        .is_err()
    );
    assert!(fake.calls.borrow().is_empty());
    fs2::FileExt::unlock(&guard).unwrap();
    drop(guard);
    let result = device::add(
        &path,
        "deck_new_ip",
        "192.168.1.9",
        "deck",
        &mut Prompts::new(&[&fingerprint]),
        &fake,
    )
    .unwrap();
    assert_eq!(result["paired"], false);
    assert!(!fake.calls.borrow().contains(&"pair"));
    assert_eq!(
        project::load(&path).unwrap().devices["deck_new_ip"].host,
        "192.168.1.9"
    );
}
