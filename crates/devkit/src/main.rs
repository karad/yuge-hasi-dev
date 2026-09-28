//! Command-line entry point for Devkit operations.

use clap::{Args, Parser, Subcommand};
use serde_json::{Value, json};
use std::path::PathBuf;
use yuge_hasi_devkit::{
    Build, Result, client, deploy::Deployment, device, failure, invalid, lock, project, shared,
};

#[derive(Parser)]
#[command(about = "macOS tools for SteamOS development", version)]
struct Cli {
    #[command(subcommand)]
    command: TopLevelAction,
}

#[derive(Subcommand)]
enum TopLevelAction {
    Devkit(DevkitCli),
}

#[derive(Args)]
struct DevkitCli {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    project: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}

#[derive(Args)]
struct Target {
    #[arg(long)]
    host: String,
    #[arg(long)]
    login: String,
}

#[derive(Args)]
struct Input {
    #[arg(long)]
    title: String,
    #[arg(long)]
    source: PathBuf,
    #[arg(long)]
    executable: Option<String>,
    #[arg(long = "argument", allow_hyphen_values = true)]
    arguments: Vec<String>,
    #[arg(long, default_value = "default", value_parser = ["default", "sniper", "android"])]
    runtime: String,
}

impl Input {
    // Builds validated settings, deriving the executable name for Android APKs.
    fn build(self) -> Result<Build> {
        let executable = match self.executable {
            Some(executable) => executable,
            None if self.runtime == "android" => self
                .source
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| invalid("Expected an APK filename"))?
                .to_owned(),
            None => return Err(invalid("--executable is required for Linux builds")),
        };
        Build::new(
            self.title,
            self.source,
            executable,
            self.arguments,
            self.runtime,
        )
    }
}

#[derive(Subcommand)]
enum Action {
    Device {
        #[command(subcommand)]
        action: DeviceAction,
    },
    Deploy {
        #[arg(long)]
        device: String,
        #[arg(long)]
        source: Option<PathBuf>,
        #[arg(long)]
        check: bool,
        #[arg(long, required_unless_present = "check")]
        game_stopped: bool,
    },
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    Plan(Input),
    Init,
    Pair {
        #[arg(long)]
        host: String,
    },
    TrustHost {
        #[arg(long)]
        host: String,
        #[arg(long)]
        public_key: PathBuf,
        #[arg(long)]
        fingerprint: String,
    },
    Status(Target),
    Upload {
        #[command(flatten)]
        target: Target,
        #[command(flatten)]
        input: Input,
        #[arg(long, required = true)]
        game_stopped: bool,
    },
}

#[derive(Subcommand)]
enum ProjectAction {
    Init(Input),
    Show,
    Migrate,
}

#[derive(Subcommand)]
enum DeviceAction {
    Add {
        name: String,
        #[command(flatten)]
        target: Target,
    },
    List,
    Remove {
        name: String,
    },
}

// Dispatches commands while keeping project and standalone client setup separate.
fn run(cli: DevkitCli) -> Result<Value> {
    // Resolve project-scoped commands before client setup, which may create credentials.
    if let Action::Device { action } = cli.command {
        if cli.config.is_some() {
            return Err(invalid("--config cannot be used with device commands"));
        }
        let path = cli
            .project
            .unwrap_or_else(|| "yuge-hasi-devkit.toml".into());
        return match action {
            DeviceAction::Add { name, target } => device::add(
                &path,
                &name,
                &target.host,
                &target.login,
                &mut device::Terminal,
                &device::SystemBackend,
            ),
            DeviceAction::List => device::list(&path),
            DeviceAction::Remove { name } => device::remove(&path, &name),
        };
    }
    if let Action::Deploy {
        device,
        source,
        check,
        game_stopped,
    } = cli.command
    {
        if cli.config.is_some() {
            return Err(invalid("--config cannot be used with deploy"));
        }
        let path = cli
            .project
            .unwrap_or_else(|| "yuge-hasi-devkit.toml".into());
        return Deployment::resolve(&path, &device, source.as_deref(), check, game_stopped)?
            .execute(&client::SystemRunner);
    }
    if let Action::Project { action } = cli.command {
        if cli.config.is_some() {
            return Err(invalid("--config cannot be used with project commands"));
        }
        let path = cli
            .project
            .unwrap_or_else(|| "yuge-hasi-devkit.toml".into());
        return match action {
            ProjectAction::Init(input) => Ok(
                json!({"initialized": true, "project": project::initialize(&path, input.build()?)?}),
            ),
            ProjectAction::Show => project::show(&path),
            ProjectAction::Migrate => project::migrate(&path),
        };
    }
    if cli.project.is_some() {
        return Err(invalid(
            "--project requires a project, device, or deploy command",
        ));
    }
    if let Action::Plan(input) = cli.command {
        return Ok(serde_json::to_value(input.build()?)?);
    }
    // Standalone identity and connection commands share one serialized client directory.
    let config_path = match cli.config {
        Some(path) => path,
        None => {
            let root = shared::root()?;
            shared::prepare(&root)?;
            root.join("devkit-client-rust")
        }
    };
    let config = client::config(&config_path)?;
    let _lock = lock(&config.join("operation.lock"))?;
    match cli.command {
        Action::Init => client::init(&config),
        Action::Pair { host } => client::pair(&config, &host),
        Action::TrustHost {
            host,
            public_key,
            fingerprint,
        } => client::trust(&config, &host, &public_key, &fingerprint),
        Action::Status(target) => {
            client::Connection::new(&config, &target.host, &target.login).status()
        }
        Action::Upload {
            target,
            input,
            game_stopped,
        } => {
            if !game_stopped {
                return Err(invalid("Stop the game before upload"));
            }
            let build = input.build()?;
            client::Connection::new(&config, &target.host, &target.login).upload(&build)
        }
        _ => Ok(json!({})),
    }
}

// Prints command results or staged errors with a matching process exit status.
fn main() {
    let Cli {
        command: TopLevelAction::Devkit(cli),
    } = Cli::parse();
    let project_command = matches!(cli.command, Action::Project { .. });
    let deploy_command = matches!(cli.command, Action::Deploy { .. });
    let device_command = matches!(cli.command, Action::Device { .. });
    let stage = match &cli.command {
        Action::Project {
            action: ProjectAction::Init(_),
        } => "project_init",
        Action::Project {
            action: ProjectAction::Migrate,
        } => "project_migrate",
        _ => "project_show",
    };
    match run(cli) {
        Ok(value) => println!("{}", serde_json::to_string_pretty(&value).unwrap()),
        Err(error) => {
            if deploy_command || device_command {
                println!("{}", failure::report(&error));
            }
            if project_command {
                println!(
                    "{}",
                    json!({"error": {"code": "project_error", "stage": stage, "message": error.to_string()}})
                );
            }
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
    }
}
