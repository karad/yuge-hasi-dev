use serde_json::{Value, json};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::{
    cell::RefCell,
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use yuge_hasi_devkit::{
    Build, Result,
    client::{self, Connection, Runner},
    invalid, *,
};

// Builds a small valid x86_64 ELF payload for local build fixtures.
fn elf() -> Vec<u8> {
    let mut b = vec![0u8; 121];
    b[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    b[16..18].copy_from_slice(&2u16.to_le_bytes());
    b[18..20].copy_from_slice(&62u16.to_le_bytes());
    b[20] = 1;
    b[24..32].copy_from_slice(&0x400078u64.to_le_bytes());
    b[32..40].copy_from_slice(&64u64.to_le_bytes());
    b[52..54].copy_from_slice(&64u16.to_le_bytes());
    b[54..56].copy_from_slice(&56u16.to_le_bytes());
    b[56] = 1;
    b[64] = 1;
    b[68] = 5;
    b[80..88].copy_from_slice(&0x400000u64.to_le_bytes());
    b[96..104].copy_from_slice(&121u64.to_le_bytes());
    b[104..112].copy_from_slice(&121u64.to_le_bytes());
    b[120] = 0xc3;
    b
}

// Creates a Linux build with arguments that exercise escaping and Unicode.
fn fixture(root: &Path) -> Build {
    let source = root.join("build space 日本語");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("Game Name"), elf()).unwrap();
    Build::new(
        "YugeHasi_probe".into(),
        source,
        "Game Name".into(),
        vec!["日本語".into(), "$(literal)".into(), "a'b".into()],
        "sniper".into(),
    )
    .unwrap()
}

// Creates a minimal Android APK accepted by source validation.
fn apk_fixture(root: &Path) -> Build {
    let manifest = root.join("AndroidManifest.xml");
    fs::write(&manifest, "fixture").unwrap();
    let apk = root.join("PocketPop.apk");
    assert!(
        Command::new("/usr/bin/zip")
            .arg("-q")
            .arg(&apk)
            .arg("AndroidManifest.xml")
            .current_dir(root)
            .status()
            .unwrap()
            .success()
    );
    Build::new(
        "PocketPop".into(),
        apk,
        "PocketPop.apk".into(),
        vec![],
        "android".into(),
    )
    .unwrap()
}

// Android builds require one matching APK and a manifest, with no extra arguments.
#[test]
fn android_apk_requires_matching_filename_manifest_and_no_arguments() {
    let temp = tempfile::tempdir().unwrap();
    let build = apk_fixture(temp.path());
    assert_eq!(build.expected_architecture(), "aarch64");
    assert!(build.validate_source().is_ok());
    for executable in [
        "Other.apk",
        "../PocketPop.apk",
        "PocketPop",
        "./PocketPop.apk",
    ] {
        assert!(
            Build::new(
                build.title.clone(),
                build.source.clone(),
                executable.into(),
                vec![],
                "android".into()
            )
            .is_err()
        );
    }
    assert!(
        Build::new(
            build.title.clone(),
            build.source.clone(),
            build.argv[0].clone(),
            vec!["ignored".into()],
            "android".into()
        )
        .is_err()
    );
    fs::write(temp.path().join("Invalid.apk"), b"not a zip").unwrap();
    assert!(
        Build::new(
            "Invalid".into(),
            temp.path().join("Invalid.apk"),
            "Invalid.apk".into(),
            vec![],
            "android".into()
        )
        .is_err()
    );
}

// Android upload transfers the archive and selects the Lepton compatibility tool.
#[test]
fn android_upload_transfers_only_apk_and_registers_lepton() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let temp = tempfile::tempdir().unwrap();
    let build = apk_fixture(temp.path());
    let conf = fake_config(temp.path());
    let fake = Fake {
        calls: RefCell::new(vec![]),
        results: RefCell::new(VecDeque::from([
            reply(
                json!({"protocol":2,"architecture":"aarch64","inhibit_sentinel_present":false,"steam_ready":true,"home_base64":"L2hvbWUvZGV2"}),
            ),
            reply(json!({"protocol":2,"prepared":true})),
            Ok(vec![]),
            reply(json!({"protocol":2,"registered":true})),
        ])),
    };
    let connection = Connection {
        config: &conf,
        host: "192.168.1.2",
        login: "dev",
        runner: &fake,
    };
    assert!(connection.upload(&build).is_ok());
    let calls = fake.calls.borrow();
    assert_eq!(calls.len(), 4);
    assert!(
        calls[2]
            .1
            .iter()
            .any(|arg| arg == build.source.to_str().unwrap())
    );
    let script = String::from_utf8(calls[3].2.clone()).unwrap();
    for (name, expected) in [
        ("argv", json!(["PocketPop.apk"])),
        ("settings", json!({"steam_play":"0","compat_tool":"lepton"})),
    ] {
        let encoded = script
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{name}_data='")))
            .unwrap()
            .trim_end_matches('\'');
        let actual: Value = serde_json::from_slice(&STANDARD.decode(encoded).unwrap()).unwrap();
        assert_eq!(actual, expected);
    }
}

// An incompatible device architecture stops upload before preparation.
#[test]
fn architecture_mismatch_stops_before_prepare() {
    let temp = tempfile::tempdir().unwrap();
    let build = apk_fixture(temp.path());
    let conf = fake_config(temp.path());
    let fake = Fake {
        calls: RefCell::new(vec![]),
        results: RefCell::new(VecDeque::from([reply(
            json!({"protocol":2,"architecture":"x86_64","inhibit_sentinel_present":false,"steam_ready":true,"home_base64":"L2hvbWUvZGV2"}),
        )])),
    };
    let connection = Connection {
        config: &conf,
        host: "192.168.1.2",
        login: "dev",
        runner: &fake,
    };
    assert!(connection.upload(&build).is_err());
    assert_eq!(fake.calls.borrow().len(), 1);
}

// Title validation rejects reserved IDs and shell-sensitive characters.
#[test]
fn title_rejects_reserved_names_and_shell_syntax() {
    for name in [
        "steam",
        "STEAMVR",
        "steamdeckard",
        "a",
        "../x",
        "a b",
        "a;b",
        "a\n",
        "a-b",
        "1x",
    ] {
        assert!(validate_title(name).is_err(), "{name}");
    }
    assert!(validate_title(&"a".repeat(65)).is_err());
    assert!(validate_title("YugeHasi_probe1").is_ok());
}

// Connections accept only private IPv4 addresses and safe login names.
#[test]
fn destinations_are_explicit_private_ipv4_and_safe_logins() {
    for host in ["10.0.0.2", "172.16.0.2", "192.168.1.2", "169.254.1.2"] {
        assert!(validate_host(host).is_ok());
    }
    for host in [
        "8.8.8.8",
        "127.0.0.1",
        "0.0.0.0",
        "224.0.0.1",
        "::1",
        "deck.local",
        "-oProxyCommand=x",
    ] {
        assert!(validate_host(host).is_err());
    }
    for user in ["dev", "deck", "_user2"] {
        assert!(validate_login(user).is_ok());
    }
    for user in ["", "-root", "a;b", "a b", "a@b"] {
        assert!(validate_login(user).is_err());
    }
}

// Remote status accepts supported SteamOS CPU architectures.
#[test]
fn remote_status_accepts_steamos_cpu_architectures() {
    let temp = tempfile::tempdir().unwrap();
    let fake_uname = temp.path().join("uname");
    fs::write(
        &fake_uname,
        "#!/bin/sh\ncase \"$1\" in\n  -s) printf 'Linux\\n' ;;\n  -m) printf '%s\\n' \"$TEST_ARCHITECTURE\" ;;\nesac\n",
    )
    .unwrap();
    fs::set_permissions(&fake_uname, fs::Permissions::from_mode(0o755)).unwrap();
    for tool in ["flock", "timeout"] {
        let path = temp.path().join(tool);
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let script = remote::script("status", None).unwrap();
    for architecture in ["x86_64", "aarch64", "armv7l"] {
        let path = format!(
            "{}:{}",
            temp.path().display(),
            std::env::var("PATH").unwrap()
        );
        let output = Command::new("/bin/bash")
            .args(["--noprofile", "--norc", "-c", &script, "bash"])
            .arg(temp.path())
            .env("PATH", path)
            .env("TEST_ARCHITECTURE", architecture)
            .output()
            .unwrap();
        if architecture == "armv7l" {
            assert!(!output.status.success());
            continue;
        }
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["architecture"], architecture);
        assert_eq!(value["authenticated"], true);
    }
}

// Linux build validation rejects path escapes, wrong CPU, and linked files.
#[test]
fn build_validation_rejects_escape_wrong_cpu_and_links() {
    let temp = tempfile::tempdir().unwrap();
    let build = fixture(temp.path());
    for executable in ["../outside", "/bin/sh", ".", "missing"] {
        assert!(
            Build::new(
                build.title.clone(),
                build.source.clone(),
                executable.into(),
                vec![],
                "default".into()
            )
            .is_err()
        );
    }
    let mut broken = elf();
    broken[18] = 183;
    fs::write(build.source.join("Wrong"), broken).unwrap();
    assert!(
        Build::new(
            build.title.clone(),
            build.source.clone(),
            "Wrong".into(),
            vec![],
            "default".into()
        )
        .is_err()
    );
    symlink(temp.path(), build.source.join("link")).unwrap();
    assert!(validate_tree(&build.source).is_err());
}

// Build metadata keeps arguments separate and rejects control characters.
#[test]
fn metadata_preserves_argument_boundaries_and_rejects_controls() {
    let temp = tempfile::tempdir().unwrap();
    let mut build = fixture(temp.path());
    let copy: Build = serde_json::from_slice(&serde_json::to_vec(&build).unwrap()).unwrap();
    assert_eq!(copy.argv, build.argv);
    for arg in ["a\0b", "a\nb", "a\rb"] {
        build.argv.push(arg.into());
        assert!(build.validate_metadata().is_err());
        build.argv.pop();
    }
}

// Shell quoting preserves metacharacters as literal argument text.
#[test]
fn shell_quote_keeps_metacharacters_literal() {
    let text = "a'b $(false) 日本語";
    let out = Command::new("/bin/sh")
        .args(["-c", &format!("printf '%s' {}", quote(text))])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(out.stdout, text.as_bytes());
}

// Client configuration rejects symbolic links and shared permissions.
#[test]
fn private_config_rejects_links_and_shared_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config");
    private_dir(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(client::config(&path).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(temp.path().join("missing"), path.join("devkit_rsa")).unwrap();
    assert!(client::config(&path).is_err());
}

// Reinitialization preserves the key pair and trust refuses changed host keys.
#[test]
fn key_initialization_reuses_identity_and_trust_rejects_changes() {
    let temp = tempfile::tempdir().unwrap();
    let conf = client::config(&temp.path().join("config")).unwrap();
    client::init(&conf).unwrap();
    let key = fs::read(conf.join("devkit_rsa")).unwrap();
    client::init(&conf).unwrap();
    assert_eq!(key, fs::read(conf.join("devkit_rsa")).unwrap());
    assert_eq!(
        fs::metadata(conf.join("devkit_rsa"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let file = conf.join("devkit_rsa.pub");
    let text = fs::read_to_string(&file).unwrap();
    let fingerprint = client::fingerprint(&text).unwrap();
    assert!(client::trust(&conf, "192.168.1.2", &file, "SHA256:wrong").is_err());
    assert!(!conf.join("known_hosts").exists());
    client::trust(&conf, "192.168.1.2", &file, &fingerprint).unwrap();
    let saved = fs::read(conf.join("known_hosts")).unwrap();
    client::trust(&conf, "192.168.1.2", &file, &fingerprint).unwrap();
    assert_eq!(saved, fs::read(conf.join("known_hosts")).unwrap());
    let other = client::config(&temp.path().join("other")).unwrap();
    client::init(&other).unwrap();
    let other_file = other.join("devkit_rsa.pub");
    let fingerprint = client::fingerprint(&fs::read_to_string(&other_file).unwrap()).unwrap();
    assert!(client::trust(&conf, "192.168.1.2", &other_file, &fingerprint).is_err());
    assert_eq!(saved, fs::read(conf.join("known_hosts")).unwrap());
}

// Missing halves of a key pair require explicit recovery.
#[test]
fn incomplete_identity_is_not_silently_replaced() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("devkit_rsa"), "existing").unwrap();
    assert!(client::init(temp.path()).is_err());
    assert_eq!(
        fs::read_to_string(temp.path().join("devkit_rsa")).unwrap(),
        "existing"
    );
}

// Public-key parsing rejects malformed encoding, mismatched types, and extra keys.
#[test]
fn public_key_rejects_wrong_encoding_type_and_multiple_keys() {
    for key in [
        "",
        "ssh-ed25519 !!!",
        "ssh-ed25519 YQ==",
        "ssh-ed25519 AAAAB3NzaC1yc2E=",
        "ssh-rsa abc\nssh-rsa def",
    ] {
        assert!(client::public_key(key).is_err());
    }
}

// SSH uses pinned keys and ignores ambient user configuration.
#[test]
fn ssh_configuration_enforces_pinned_keys_and_ignores_user_config() {
    let temp = tempfile::tempdir().unwrap();
    let args = client::ssh_args(temp.path()).unwrap();
    for option in [
        "StrictHostKeyChecking=yes",
        "BatchMode=yes",
        "IdentityAgent=none",
        "IdentitiesOnly=yes",
        "GlobalKnownHostsFile=/dev/null",
    ] {
        assert!(args.contains(&option.to_string()));
    }
    assert_eq!(&args[..2], &["-F", "/dev/null"]);
    assert!(
        args.iter()
            .any(|s| s.starts_with("UserKnownHostsFile=") && s.ends_with("known_hosts"))
    );
}

type Invocation = (String, Vec<String>, Vec<u8>);
struct Fake {
    calls: RefCell<Vec<Invocation>>,
    results: RefCell<VecDeque<Result<Vec<u8>>>>,
}

impl Runner for Fake {
    // Records commands and returns queued responses without contacting a device.
    fn run(&self, command: &mut Command, input: &[u8]) -> Result<Vec<u8>> {
        self.calls.borrow_mut().push((
            command.get_program().to_string_lossy().into(),
            command
                .get_args()
                .map(|s| s.to_string_lossy().into())
                .collect(),
            input.to_vec(),
        ));
        self.results
            .borrow_mut()
            .pop_front()
            .expect("Unexpected command")
    }
}

// Encodes a fake remote response in the expected JSON format.
fn reply(value: Value) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&value).unwrap())
}

// Creates the private client files needed by connection tests.
fn fake_config(root: &Path) -> PathBuf {
    let path = root.join("config");
    private_dir(&path).unwrap();
    fs::write(path.join("devkit_rsa"), "fixture").unwrap();
    fs::write(path.join("known_hosts"), "192.168.1.2 fixture key\n").unwrap();
    path
}

// A failed upload skips registration; success retains argument boundaries.
#[test]
fn upload_failure_never_registers_and_success_preserves_arguments() {
    for fail in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let build = fixture(temp.path());
        let conf = fake_config(temp.path());
        let fake = Fake {
            calls: RefCell::new(vec![]),
            results: RefCell::new(VecDeque::from([
                reply(
                    json!({"protocol":2,"architecture":"x86_64","inhibit_sentinel_present":false,"steam_ready":true,"home_base64":"L2hvbWUvZGV2"}),
                ),
                reply(json!({"protocol":2,"prepared":true})),
                if fail {
                    Err(invalid("Transfer failed"))
                } else {
                    Ok(vec![])
                },
                reply(json!({"protocol":2,"registered":true,"launch_verified":false})),
            ])),
        };
        let connection = Connection {
            config: &conf,
            host: "192.168.1.2",
            login: "dev",
            runner: &fake,
        };
        assert_eq!(connection.upload(&build).is_ok(), !fail);
        let calls = fake.calls.borrow();
        assert_eq!(calls.len(), if fail { 3 } else { 4 });
        assert_eq!(calls[2].0, "/usr/bin/rsync");
        assert!(!calls[2].1.iter().any(|s| s.starts_with("--delete")));
        assert_eq!(calls[1].1.last().unwrap(), remote::COMMAND);
        assert_eq!(
            calls[1].2,
            remote::script("prepare", Some(&build)).unwrap().as_bytes()
        );
        if !fail {
            assert_eq!(
                calls[3].2,
                remote::script("register", Some(&build)).unwrap().as_bytes()
            );
        }
    }
}

// Session inhibition and protocol mismatches stop before transfer.
#[test]
fn sentinel_and_invalid_protocol_stop_before_transfer() {
    for state in [
        json!({"protocol":2,"inhibit_sentinel_present":true}),
        json!({"protocol":1}),
        json!({"protocol":2,"inhibit_sentinel_present":false,"steam_ready":false}),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let build = fixture(temp.path());
        let conf = fake_config(temp.path());
        let fake = Fake {
            calls: RefCell::new(vec![]),
            results: RefCell::new(VecDeque::from([reply(state)])),
        };
        assert!(
            Connection {
                config: &conf,
                host: "192.168.1.2",
                login: "dev",
                runner: &fake
            }
            .upload(&build)
            .is_err()
        );
        assert_eq!(fake.calls.borrow().len(), 1);
    }
}

// An unpinned host is rejected before any external command runs.
#[test]
fn untrusted_host_never_launches_a_process() {
    let temp = tempfile::tempdir().unwrap();
    let conf = fake_config(temp.path());
    let fake = Fake {
        calls: RefCell::new(vec![]),
        results: RefCell::new(VecDeque::new()),
    };
    assert!(
        Connection {
            config: &conf,
            host: "192.168.1.3",
            login: "dev",
            runner: &fake
        }
        .status()
        .is_err()
    );
    assert!(fake.calls.borrow().is_empty());
}

// CLI planning and invalid input return the expected status and JSON.
#[test]
fn standalone_plan_and_invalid_input_have_correct_exit_codes() {
    let temp = tempfile::tempdir().unwrap();
    let build = fixture(temp.path());
    let output = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .args(["devkit", "plan", "--title", "YugeHasi_probe", "--source"])
        .arg(&build.source)
        .args(["--executable", "Game Name", "--argument=$(literal)"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["argv"][1],
        "$(literal)"
    );
    let failed = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .args(["devkit", "plan", "--title", "steam", "--source"])
        .arg(&build.source)
        .args(["--executable", "Game Name"])
        .output()
        .unwrap();
    assert!(!failed.status.success());
}

// A local rsync run preserves Unicode paths and surfaces process errors.
#[test]
fn actual_rsync_handles_unicode_updates_and_process_failure() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source space");
    let dest = temp.path().join("dest");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&dest).unwrap();
    fs::write(source.join("日本語.txt"), "first").unwrap();
    for value in ["first", "second"] {
        fs::write(source.join("日本語.txt"), value).unwrap();
        client::execute(
            Command::new("/usr/bin/rsync")
                .args(["-a", "--checksum", "--chmod=Du=rwx,Dgo=,Fu=rwx,Fgo="])
                .arg(format!("{}/", source.display()))
                .arg(&dest),
            b"",
        )
        .unwrap();
        assert_eq!(fs::read_to_string(dest.join("日本語.txt")).unwrap(), value);
    }
    assert!(client::execute(Command::new("/bin/sh").args(["-c", "exit 23"]), b"").is_err());
}

// Remote rsync paths retain spaces and quotes as literal characters.
#[test]
fn client_rsync_quotes_remote_paths_with_spaces_and_quotes() {
    struct LocalTransport(PathBuf);
    impl Runner for LocalTransport {
        // Substitutes a local transport while preserving rsync's generated arguments.
        fn run(&self, command: &mut Command, input: &[u8]) -> Result<Vec<u8>> {
            assert_eq!(command.get_program(), "/usr/bin/rsync");
            let mut args: Vec<_> = command.get_args().map(|a| a.to_os_string()).collect();
            let index = args.iter().position(|a| a == "-e").unwrap();
            args[index + 1] = self.0.as_os_str().to_owned();
            client::execute(Command::new("/usr/bin/rsync").args(args), input)
        }
    }
    let temp = tempfile::tempdir().unwrap();
    let config = fake_config(temp.path());
    let build = fixture(temp.path());
    let dest = temp.path().join("remote space ' 日本語");
    fs::create_dir(&dest).unwrap();
    let shell = temp.path().join("transport.sh");
    fs::write(&shell, "#!/bin/sh\nset -eu\nif [ \"$1\" = -l ]; then shift 2; fi\ncase \"$1\" in 192.168.1.2|dev@192.168.1.2) shift;; *) exit 23;; esac\n[ \"$1\" = rsync ] || exit 23\nshift\nexec /bin/sh -c \"/usr/bin/rsync $*\"\n").unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    let runner = LocalTransport(shell);
    Connection {
        config: &config,
        host: "192.168.1.2",
        login: "dev",
        runner: &runner,
    }
    .rsync(&build.source, &format!("{}/", dest.display()), true)
    .unwrap();
    assert_eq!(fs::read(dest.join("Game Name")).unwrap(), elf());
}

// Remote scripts preserve JSON launch data without installing a helper.
#[test]
fn remote_payload_preserves_json_and_never_installs_a_helper() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let temp = tempfile::tempdir().unwrap();
    let build = fixture(temp.path());
    let script = remote::script("register", Some(&build)).unwrap();
    let encoded = script
        .lines()
        .find_map(|line| line.strip_prefix("argv_data='"))
        .unwrap()
        .trim_end_matches('\'');
    let args: Vec<String> = serde_json::from_slice(&STANDARD.decode(encoded).unwrap()).unwrap();
    assert_eq!(&args[1..], &build.argv[1..]);
    assert_eq!(args[0], "./Game Name");
    assert!(!script.contains("python"));
    assert!(!script.contains(".local/lib/yuge-hasi"));
    assert!(remote::script("unknown", None).is_err());
    assert!(remote::script("register", None).is_err());
    let syntax = client::execute(Command::new("/bin/bash").arg("-n"), script.as_bytes());
    assert!(syntax.is_ok());
    let help = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .args(["devkit", "--help"])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&help.stdout).contains("sync-utils"));
}

// Device home paths cannot be root, relative, or escape through parent segments.
#[test]
fn remote_home_rejects_invalid_and_escaping_paths() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    for home in [
        "",
        "/",
        "relative",
        "/home/../elsewhere",
        "/home/a\n",
        "/home/a/",
    ] {
        assert!(remote::decode_home(&json!({"home_base64":STANDARD.encode(home)})).is_err());
    }
    let home = "/home/space ' 日本語";
    assert_eq!(
        remote::decode_home(&json!({"home_base64":STANDARD.encode(home)})).unwrap(),
        home
    );
    assert!(remote::decode_home(&json!({"home_base64":"!"})).is_err());
    assert!(remote::decode_home(&json!({})).is_err());
}
