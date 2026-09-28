use crate::{Build, Result, invalid, quote};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;

/// Starts Bash without startup files, reading the supplied script from SSH input.
/// Passes the remote home directory as the script's first argument.
pub const COMMAND: &str = "exec /bin/bash --noprofile --norc -s -- \"$HOME\"";

// Shared safety checks and helpers prepended to every remote operation script.
const COMMON: &str = include_str!("remote/common.sh");

/// Builds a status, prepare, or register script for the remote protocol.
pub fn script(operation: &str, build: Option<&Build>) -> Result<String> {
    let body = match operation {
        "status" => include_str!("remote/status.sh"),
        "prepare" => include_str!("remote/prepare.sh"),
        "register" => include_str!("remote/register.sh"),
        _ => return Err(invalid("Unknown remote operation")),
    };
    let mut script = COMMON.to_string();
    if operation != "status" {
        let build = build.ok_or_else(|| invalid("Missing build"))?;
        build.validate_metadata()?;
        // Metadata crosses a shell boundary, so validate it before quoting assignments.
        script.push_str(&format!(
            "\ntitle={}\nexecutable={}\nruntime={}\n",
            quote(&build.title),
            quote(&build.argv[0]),
            quote(&build.runtime)
        ));
        if operation == "register" {
            let argv: Vec<_> = if build.runtime == "android" {
                vec![build.argv[0].clone()]
            } else {
                std::iter::once(format!("./{}", build.argv[0].trim_start_matches("./")))
                    .chain(build.argv.iter().skip(1).cloned())
                    .collect()
            };
            let mut settings = json!({"steam_play":"0"});
            if build.runtime == "sniper" {
                settings["compat_tool"] = json!("SteamLinuxRuntime_sniper");
            } else if build.runtime == "android" {
                settings["compat_tool"] = json!("lepton");
            }
            for (name, value) in [
                ("argv", json!(argv)),
                ("env", json!({})),
                ("settings", settings),
            ] {
                // Base64 keeps JSON argument boundaries intact inside shell assignments.
                script.push_str(&format!(
                    "{name}_data={}\n",
                    quote(&STANDARD.encode(serde_json::to_vec(&value)?))
                ));
            }
        }
    }
    script.push_str(body);
    Ok(script)
}

/// Decodes and validates the home directory reported by a device.
pub fn decode_home(value: &serde_json::Value) -> Result<String> {
    let data = value["home_base64"]
        .as_str()
        .ok_or_else(|| invalid("Missing device home"))?;
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| invalid("Invalid device home encoding"))?;
    let home = String::from_utf8(bytes).map_err(|_| invalid("Invalid device home"))?;
    if !home.starts_with('/')
        || home == "/"
        || home.ends_with('/')
        || home.chars().any(char::is_control)
        || home.split('/').any(|part| part == "..")
    {
        return Err(invalid("Invalid device home"));
    }
    Ok(home)
}
