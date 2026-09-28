use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    process::Command,
};
use yuge_hasi_devkit::{
    Build,
    project::{self, Project},
    shared,
};

// Supplies legacy project settings for schema and migration tests.
fn config() -> &'static str {
    r#"schema_version = 1
auth_dir = ".runtime/devkit-client-rust"
[game]
title = "pocket_pop"
source = "Build/Linux"
executable = "Game"
arguments = ["$(literal)", "日本語"]
runtime = "default"
[devices.deck]
host = "192.168.1.2"
login = "deck"
"#
}

// Writes an owner-only project file matching the production permission rules.
fn write_config(path: &std::path::Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

// Creates a minimal Linux build accepted by project initialization.
fn build(root: &std::path::Path) -> Build {
    let source = root.join("Build space");
    fs::create_dir(&source).unwrap();
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
    fs::write(source.join("Game"), b).unwrap();
    Build::new(
        "pocket_pop".into(),
        source,
        "Game".into(),
        vec!["日本語".into()],
        "default".into(),
    )
    .unwrap()
}

// Both supported schemas reject unknown or inconsistent settings.
#[test]
fn schema_and_nested_values_are_strict() {
    assert!(Project::parse(config()).is_ok());
    let shared_config = config()
        .replace(
            "schema_version = 1\nauth_dir = \".runtime/devkit-client-rust\"\n",
            "schema_version = 2\n",
        )
        .replace(
            "[devices.deck]\nhost = \"192.168.1.2\"\nlogin = \"deck\"\n",
            "",
        );
    assert!(Project::parse(&shared_config).is_ok());
    for text in [
        config().replace("schema_version = 1", "schema_version = 3"),
        config().replace("schema_version = 1", "schema_version = 2"),
        config().replace("schema_version = 1", "schema_version = 1\nextra = 1"),
        config().replace("title =", "unknown = 1\ntitle ="),
        config().replace("login =", "unknown = 1\nlogin ="),
        config().replace("runtime = \"default\"", "runtime = \"bad\""),
        config().replace("192.168.1.2", "8.8.8.8"),
        config().replace("executable = \"Game\"", "executable = \"../Game\""),
        config().replace("source = \"Build/Linux\"", "source = \"\""),
        config().replace(
            "schema_version = 1",
            "schema_version = 1\nschema_version = 1",
        ),
    ] {
        assert!(Project::parse(&text).is_err(), "{text}");
    }
}

// Device names accept only the registry's bounded lowercase format.
#[test]
fn device_name_boundaries() {
    for s in ["", "Deck", "1deck", "a.b", "a b", &"a".repeat(33)] {
        assert!(project::validate_device_name(s).is_err());
    }
    for s in ["a", "deck-2_a", &"a".repeat(32)] {
        assert!(project::validate_device_name(s).is_ok());
    }
}

// Initialization writes a private schema v2 file once and preserves relative paths.
#[test]
fn init_preserves_existing_file_and_roundtrips_relative_source() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let input = build(&root);
    let path = root.join("yuge-hasi-devkit.toml");
    project::initialize(&path, input.clone()).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(project::initialize(&path, input).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    let saved = project::load(&path).unwrap();
    assert_eq!(saved.schema_version, 2);
    assert!(saved.auth_dir.as_os_str().is_empty());
    assert!(saved.devices.is_empty());
    assert_eq!(saved.game.source.to_str(), Some("Build space"));
    assert_eq!(saved.game.arguments, ["日本語"]);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(!root.join(".runtime").exists());
}

// Separate projects resolve the same device from the shared registry.
#[test]
fn shared_deploy_uses_one_registry_from_multiple_projects() {
    use yuge_hasi_devkit::deploy::Deployment;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("shared");
    shared::prepare(&root).unwrap();
    let (mut registry, original) = shared::load(&root).unwrap();
    registry.devices.insert(
        "deck".into(),
        project::Device {
            host: "192.168.1.2".into(),
            login: "deck".into(),
        },
    );
    shared::save(&root, &registry, original.as_deref()).unwrap();
    for name in ["first", "second"] {
        let project_dir = dir.path().join(name);
        fs::create_dir(&project_dir).unwrap();
        let input = build(&project_dir);
        let path = project_dir.join("project.toml");
        project::initialize(&path, input).unwrap();
        let deploy = Deployment::resolve_in(&path, "deck", None, true, false, Some(&root)).unwrap();
        assert_eq!(deploy.host, "192.168.1.2");
        assert_eq!(deploy.auth_dir, root.join("devkit-client-rust"));
    }
}

// Registry writes detect stale snapshots and reject unsafe file types or modes.
#[test]
fn shared_registry_rejects_conflicting_updates_and_links() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("shared");
    shared::prepare(&root).unwrap();
    let (mut registry, original) = shared::load(&root).unwrap();
    registry.devices.insert(
        "deck".into(),
        project::Device {
            host: "192.168.1.2".into(),
            login: "deck".into(),
        },
    );
    shared::save(&root, &registry, original.as_deref()).unwrap();
    assert!(shared::save(&root, &registry, original.as_deref()).is_err());
    fs::set_permissions(root.join("devices.toml"), fs::Permissions::from_mode(0o644)).unwrap();
    assert!(shared::load(&root).is_err());
    fs::remove_file(root.join("devices.toml")).unwrap();
    let outside = dir.path().join("outside");
    fs::write(&outside, "").unwrap();
    symlink(&outside, root.join("devices.toml")).unwrap();
    assert!(shared::load(&root).is_err());
}

// Migration copies trusted identity and devices without replacing the original key.
#[test]
fn migrate_keeps_the_client_identity_and_moves_registered_devices() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let project_dir = dir.path().join("game");
    fs::create_dir(&project_dir).unwrap();
    let path = project_dir.join("project.toml");
    write_config(&path, config());
    let auth =
        yuge_hasi_devkit::client::config(&project_dir.join(".runtime/devkit-client-rust")).unwrap();
    yuge_hasi_devkit::client::init(&auth).unwrap();
    let public = fs::read_to_string(auth.join("devkit_rsa.pub")).unwrap();
    let fields: Vec<_> = public.split_whitespace().collect();
    write_config(
        &auth.join("known_hosts"),
        &format!("192.168.1.2 {} {}\n", fields[0], fields[1]),
    );
    let before = fs::read(auth.join("devkit_rsa")).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .env("HOME", &home)
        .current_dir(&project_dir)
        .args(["devkit", "--project", "project.toml", "project", "migrate"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let root = home.join(".yuge-hasi");
    assert_eq!(
        fs::read(root.join("devkit-client-rust/devkit_rsa")).unwrap(),
        before
    );
    assert_eq!(
        shared::load(&root).unwrap().0.devices["deck"].host,
        "192.168.1.2"
    );
    let migrated = project::load(&path).unwrap();
    assert_eq!(migrated.schema_version, 2);
    assert!(migrated.devices.is_empty());
    assert!(migrated.auth_dir.as_os_str().is_empty());
    assert!(auth.join("devkit_rsa").exists());
}

// Standalone setup stores credentials under the user's shared directory.
#[test]
fn standalone_init_uses_home_shared_directory_instead_of_current_project() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let game = dir.path().join("game");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&game).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .env("HOME", &home)
        .current_dir(&game)
        .args(["devkit", "init"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        home.join(".yuge-hasi/devkit-client-rust/devkit_rsa")
            .exists()
    );
    assert!(!game.join(".runtime").exists());
}

// Conflicting shared device names leave the legacy project untouched.
#[test]
fn migrate_rejects_shared_device_name_conflicts_without_changing_project() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let path = dir.path().join("project.toml");
    write_config(&path, config());
    let before = fs::read(&path).unwrap();
    let root = home.join(".yuge-hasi");
    shared::prepare(&root).unwrap();
    let (mut registry, original) = shared::load(&root).unwrap();
    registry.devices.insert(
        "deck".into(),
        project::Device {
            host: "192.168.1.3".into(),
            login: "deck".into(),
        },
    );
    shared::save(&root, &registry, original.as_deref()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .env("HOME", &home)
        .current_dir(dir.path())
        .args(["devkit", "--project", "project.toml", "project", "migrate"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!root.join("devkit-client-rust").exists());
}

// Showing a project resolves paths without creating build or credential files.
#[test]
fn show_resolves_against_project_without_requiring_build_or_auth() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let path = root.join("yuge-hasi-devkit.toml");
    write_config(&path, config());
    let output = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .current_dir("/")
        .args(["devkit", "project", "show", "--project"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        value["settings"]["game"]["source"],
        root.join("Build/Linux").to_str().unwrap()
    );
    assert_eq!(
        value["settings"]["auth_dir"],
        root.join(".runtime/devkit-client-rust").to_str().unwrap()
    );
    assert!(!root.join(".runtime").exists());
}

// Project files require safe file modes, no links, and an available operation lock.
#[test]
fn links_permissions_and_lock_contention_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.toml");
    write_config(&path, config());
    let link = dir.path().join("link.toml");
    symlink(&path, &link).unwrap();
    assert!(project::load(&link).is_err());
    assert!(project::initialize(&link, build(dir.path())).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(project::load(&path).is_err());
    let target = dir.path().join("new.toml");
    let _lock = yuge_hasi_devkit::lock(&dir.path().join("new.toml.lock")).unwrap();
    let input = Build {
        title: "pocket_pop".into(),
        source: dir.path().join("Build space"),
        argv: vec!["Game".into()],
        runtime: "default".into(),
    };
    assert!(project::initialize(&target, input).is_err());
    assert!(!target.exists());
}

// Invalid builds and mixed CLI settings fail before creating a project.
#[test]
fn invalid_build_leaves_no_project_and_cli_rejects_config_mix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("project.toml");
    let invalid = Build {
        title: "pocket_pop".into(),
        source: dir.path().into(),
        argv: vec![],
        runtime: "default".into(),
    };
    assert!(project::initialize(&path, invalid).is_err());
    assert!(!path.exists());
    let output = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .current_dir(dir.path())
        .args(["devkit", "--config", "auth", "project", "show"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["error"]["stage"], "project_show");
    assert!(!dir.path().join("auth").exists());
}

// Project CLI round-trips launch arguments without initializing SSH identity.
#[test]
fn cli_init_and_show_preserve_arguments_and_do_not_touch_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let input = build(dir.path());
    let output = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .current_dir(dir.path())
        .args([
            "devkit",
            "project",
            "init",
            "--title",
            "pocket_pop",
            "--source",
        ])
        .arg(&input.source)
        .args(["--executable", "Game", "--argument=$(literal)"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(value["initialized"], true);
    let path = dir.path().join("yuge-hasi-devkit.toml");
    assert_eq!(project::load(&path).unwrap().game.arguments, ["$(literal)"]);
    assert!(!dir.path().join(".runtime").exists());
    let mut saved = Project::parse(config()).unwrap();
    saved.game.source = input.source.clone();
    saved.auth_dir = dir.path().join("credentials");
    write_config(&path, &toml::to_string(&saved).unwrap());
    let shown = project::show(&path).unwrap();
    assert_eq!(
        shown["settings"]["game"]["source"],
        input.source.to_str().unwrap()
    );
    assert_eq!(
        shown["settings"]["auth_dir"],
        saved.auth_dir.to_str().unwrap()
    );
}

struct DeployRunner {
    scripts: std::cell::RefCell<Vec<Vec<u8>>>,
    replies: std::cell::RefCell<std::collections::VecDeque<yuge_hasi_devkit::Result<Vec<u8>>>>,
}

impl yuge_hasi_devkit::client::Runner for DeployRunner {
    // Records deployment commands and supplies queued remote responses.
    fn run(&self, _: &mut Command, input: &[u8]) -> yuge_hasi_devkit::Result<Vec<u8>> {
        self.scripts.borrow_mut().push(input.to_vec());
        self.replies
            .borrow_mut()
            .pop_front()
            .expect("Unexpected remote operation")
    }
}

// Returns a device status accepted by readiness checks.
fn ready_reply() -> yuge_hasi_devkit::Result<Vec<u8>> {
    Ok(br#"{"protocol":2,"architecture":"x86_64","authenticated":true,"steam_ready":true,"inhibit_sentinel_present":false,"home_base64":"L2hvbWUvZGVjaw=="}"#.to_vec())
}

// Creates the local project and credentials used by deployment tests.
fn deployment_fixture(root: &std::path::Path) -> std::path::PathBuf {
    let input = build(root);
    let mut saved = Project::parse(config()).unwrap();
    saved.game.source = input.source;
    saved.auth_dir = "auth".into();
    let auth = root.join("auth");
    yuge_hasi_devkit::private_dir(&auth).unwrap();
    write_config(&auth.join("devkit_rsa"), "fixture");
    write_config(&auth.join("known_hosts"), "192.168.1.2 fixture key\n");
    let path = root.join("yuge-hasi-devkit.toml");
    write_config(&path, &toml::to_string(&saved).unwrap());
    path
}

// Deployment resolution can override the build source without writing settings.
#[test]
fn deploy_resolves_device_and_source_override_without_saving() {
    use yuge_hasi_devkit::deploy::Deployment;
    let dir = tempfile::tempdir().unwrap();
    let path = deployment_fixture(dir.path());
    let before = fs::read(&path).unwrap();
    let selected = Deployment::resolve(&path, "deck", None, false, true).unwrap();
    assert_eq!(selected.host, "192.168.1.2");
    assert_eq!(selected.build.argv, ["Game", "$(literal)", "日本語"]);
    assert!(Deployment::resolve(&path, "missing", None, true, false).is_err());
    assert!(Deployment::resolve(&path, "deck", None, false, false).is_err());
    let other = tempfile::tempdir().unwrap();
    let input = build(other.path());
    let relative = std::env::current_dir()
        .unwrap()
        .strip_prefix("/")
        .unwrap()
        .components()
        .map(|_| "..")
        .collect::<std::path::PathBuf>()
        .join(input.source.strip_prefix("/").unwrap());
    let overridden = Deployment::resolve(&path, "deck", Some(&relative), true, false).unwrap();
    assert_eq!(overridden.build.source, input.source);
    assert_eq!(fs::read(&path).unwrap(), before);
}

// Readiness checks leave project settings and client identity unchanged.
#[test]
fn deploy_check_only_reads_remote_status_and_preserves_local_settings_and_keys() {
    use yuge_hasi_devkit::deploy::Deployment;
    let dir = tempfile::tempdir().unwrap();
    let path = deployment_fixture(dir.path());
    let paths = [
        path.clone(),
        dir.path().join("auth/devkit_rsa"),
        dir.path().join("auth/known_hosts"),
    ];
    let before: Vec<_> = paths.iter().map(|p| fs::read(p).unwrap()).collect();
    let runner = DeployRunner {
        scripts: Default::default(),
        replies: std::cell::RefCell::new([ready_reply()].into()),
    };
    let result = Deployment::resolve(&path, "deck", None, true, false)
        .unwrap()
        .execute(&runner)
        .unwrap();
    assert_eq!(result["checked"], true);
    assert!(result.get("registered").is_none());
    assert_eq!(
        *runner.scripts.borrow(),
        [yuge_hasi_devkit::remote::script("status", None)
            .unwrap()
            .into_bytes()]
    );
    for (p, data) in paths.iter().zip(before) {
        assert_eq!(fs::read(p).unwrap(), data);
    }
}

// Upload failures stop before later transfer or registration stages.
#[test]
fn deploy_reuses_upload_and_stops_at_each_failed_stage() {
    use yuge_hasi_devkit::{deploy::Deployment, failure, invalid};
    for fail_at in 0..=4 {
        let dir = tempfile::tempdir().unwrap();
        let path = deployment_fixture(dir.path());
        let mut replies = vec![
            ready_reply(),
            Ok(br#"{"protocol":2,"prepared":true}"#.to_vec()),
            Ok(vec![]),
            Ok(br#"{"protocol":2,"registered":true}"#.to_vec()),
        ];
        if fail_at < 4 {
            replies[fail_at] = Err(invalid("Injected failure"));
        }
        let runner = DeployRunner {
            scripts: Default::default(),
            replies: std::cell::RefCell::new(replies.into()),
        };
        let result = Deployment::resolve(&path, "deck", None, false, true)
            .unwrap()
            .execute(&runner);
        if fail_at == 4 {
            let value = result.unwrap();
            assert_eq!(value["registered"], true);
            assert_eq!(value["launch_verified"], false);
            assert_eq!(value["device"], "deck");
        } else {
            let report = failure::report(&result.unwrap_err());
            assert_eq!(
                report["error"]["stage"],
                ["connection", "prepare", "transfer", "register"][fail_at]
            );
            assert_eq!(
                report["error"].get("partial_update_possible").is_some(),
                fail_at >= 2
            );
        }
        assert_eq!(runner.scripts.borrow().len(), (fail_at + 1).min(4));
    }
}

// An unready device or missing identity fails a check without creating credentials.
#[test]
fn deploy_check_rejects_unready_steam_and_missing_auth_without_initializing() {
    use yuge_hasi_devkit::{deploy::Deployment, failure};
    let dir = tempfile::tempdir().unwrap();
    let path = deployment_fixture(dir.path());
    let runner = DeployRunner {
        scripts: Default::default(),
        replies: std::cell::RefCell::new(
            [Ok(
                br#"{"protocol":2,"architecture":"x86_64","steam_ready":false,"inhibit_sentinel_present":false}"#.to_vec(),
            )]
            .into(),
        ),
    };
    let deploy = Deployment::resolve(&path, "deck", None, true, false).unwrap();
    let report = failure::report(&deploy.execute(&runner).unwrap_err());
    assert_eq!(report["error"]["code"], "steam_not_ready");
    fs::remove_dir_all(dir.path().join("auth")).unwrap();
    assert_eq!(
        failure::report(&deploy.execute(&runner).unwrap_err())["error"]["stage"],
        "auth"
    );
    assert!(!dir.path().join("auth").exists());
    assert_eq!(runner.scripts.borrow().len(), 1);
}

// Deploy CLI validates required arguments and reports project failures.
#[test]
fn deploy_cli_requires_device_and_stop_confirmation_and_reports_project_errors() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        vec!["deploy", "--game-stopped"],
        vec!["deploy", "--device", "deck"],
    ] {
        let result = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
            .current_dir(dir.path())
            .arg("devkit")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(2));
    }
    let result = Command::new(env!("CARGO_BIN_EXE_yuge-hasi"))
        .current_dir(dir.path())
        .args(["devkit", "deploy", "--device", "deck", "--check"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["error"]["stage"], "project");
    assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
}
