//! medrec: a household's medical records in a plain Drive folder.
//!
//! This first commit carries the family surface only (`config`, `auth
//! status`, `self-update`, `completions`, `info`); the records commands
//! arrive by pull request.

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_auth::AuthStatus;
use pk_cli_config::ConfigStore;
use pk_cli_core::info::{AuthInfo, CliInfo};
use pk_cli_core::{output, CliError, CommonArgs};
use pk_cli_selfupdate::{SelfUpdateArgs, Updater};

const BIN: &str = "medrec";
const REPO: &str = "piekstra/medrec-cli";

/// A household's medical records in a plain Drive folder (piekstra-cli/1).
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
struct Cli {
    #[command(flatten)]
    common: CommonArgs,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// medrec holds no credentials (auth-status/v1).
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Non-secret settings.
    #[command(subcommand)]
    Config(ConfigCmd),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
}

#[derive(Subcommand, Debug)]
enum AuthCmd {
    /// Report credential state: none is needed.
    Status,
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the config file path.
    Path,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        std::process::exit(output::fail(&e, cli.common.json));
    }
}

fn run(cli: &Cli) -> Result<(), CliError> {
    match &cli.command {
        Command::Auth(AuthCmd::Status) => {
            AuthStatus::not_required().emit(cli.common.json);
            Ok(())
        }
        Command::Config(ConfigCmd::Path) => {
            let path = ConfigStore::new(BIN).path()?;
            output::emit(
                cli.common.json,
                "config-path",
                serde_json::json!({ "path": path.display().to_string() }),
                |v| println!("{}", v["path"].as_str().unwrap_or("")),
            );
            Ok(())
        }
        Command::SelfUpdate(args) => Updater {
            repo: REPO.into(),
            binary: BIN.into(),
            target: env!("BUILD_TARGET").into(),
            current: env!("CARGO_PKG_VERSION").into(),
        }
        .run(args, cli.common.json, cli.common.quiet),
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), BIN, &mut std::io::stdout());
            Ok(())
        }
        Command::Info => {
            let info = CliInfo::new(
                BIN,
                env!("CARGO_PKG_VERSION"),
                &format!("https://github.com/{REPO}"),
                AuthInfo {
                    required: false,
                    method: "none".into(),
                    login_hint: None,
                },
                &[],
            );
            output::json(&serde_json::to_value(&info).expect("CliInfo serializes to JSON"));
            Ok(())
        }
    }
}
