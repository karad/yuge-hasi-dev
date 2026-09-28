use base64::{Engine, engine::general_purpose::STANDARD};
use std::{path::PathBuf, process::Command};
use yuge_hasi_devkit::{Build, client, quote, remote};

// A paired SteamOS device must execute the standard remote scripts without installed helpers.
#[test]
#[ignore = "Requires an explicitly selected, paired SteamOS device; uses only an isolated temporary directory"]
fn standard_commands_obey_remote_contract_without_installed_code() {
    let host = std::env::var("YUGE_HASI_TEST_HOST").expect("Set YUGE_HASI_TEST_HOST");
    let login = std::env::var("YUGE_HASI_TEST_LOGIN").expect("Set YUGE_HASI_TEST_LOGIN");
    let config = client::config(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../.runtime/devkit-client-rust")
            .canonicalize()
            .unwrap(),
    )
    .unwrap();
    let connection = client::Connection::new(&config, &host, &login);
    connection.check().unwrap();
    let build = Build {
        title: "YugeHasi_contract_probe".into(),
        source: PathBuf::from("unused"),
        argv: vec!["Game Name".into(), "日本語".into(), "a'b $(literal)".into()],
        runtime: "sniper".into(),
    };
    let mut script = String::from(
        "set -euo pipefail\nsandbox=$(mktemp -d /tmp/yuge-hasi-contract.XXXXXXXX)\ntrap 'rm -rf -- \"$sandbox\"' EXIT\ntest_home=\"$sandbox/home space '\u{65e5}\u{672c}\u{8a9e}\"\nmkdir -- \"$test_home\"\n",
    );
    for operation in ["status", "prepare", "register"] {
        script.push_str(&format!("run_{operation}() {{ printf '%s' {} | base64 --decode | /bin/bash --noprofile --norc -s -- \"$test_home\"; }}\n", quote(&STANDARD.encode(remote::script(operation, Some(&build)).unwrap()))));
    }
    script.push_str(include_str!("remote_contract.sh"));
    let result = client::execute(
        Command::new("/usr/bin/ssh")
            .args(client::ssh_args(&config).unwrap())
            .arg(format!("{login}@{host}"))
            .arg("exec /bin/bash --noprofile --norc -s"),
        script.as_bytes(),
    );
    let output = String::from_utf8(result.unwrap()).unwrap();
    println!("{output}");
    assert!(output.contains("REMOTE_CONTRACT_OK"));
    let encoded = output
        .lines()
        .find_map(|line| line.strip_prefix("ARGV_BASE64="))
        .unwrap();
    let args: Vec<String> = serde_json::from_slice(&STANDARD.decode(encoded).unwrap()).unwrap();
    assert_eq!(&args[1..], &build.argv[1..]);
    assert_eq!(args[0], "./Game Name");
}
