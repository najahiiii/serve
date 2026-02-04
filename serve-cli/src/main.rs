mod cleanup;
mod config;
mod constants;
mod delete;
mod download;
mod http;
mod info;
mod list;
mod progress;
mod retry;
mod upload;

use crate::config::{AppConfig, LoadedConfig};
use crate::constants::DEFAULT_HOST;
use crate::download::ExistingFileStrategy;
use crate::retry::total_retry_sleep_seconds;
use anyhow::{Context, Result, anyhow};
use clap::{Args, Parser, Subcommand, ValueHint};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

const DEFAULT_MAX_RETRIES: usize = 10;
const VERSION_SUMMARY: &str = concat!(
    "serve-cli: ",
    env!("CARGO_PKG_VERSION"),
    "\nRust: ",
    env!("SERVE_CLI_RUSTC_VERSION"),
    "\nOS/Arch: ",
    env!("SERVE_CLI_TARGET_OS"),
    "/",
    env!("SERVE_CLI_TARGET_ARCH"),
    "\nCommit: ",
    env!("SERVE_CLI_GIT_COMMIT"),
    "\nBuilt: ",
    env!("SERVE_CLI_BUILD_TIME")
);

#[derive(Parser)]
#[command(
    name = "serve-cli",
    version = env!("CARGO_PKG_VERSION"),
    about = "CLI helper for the serve file server",
    disable_version_flag = true
)]
struct Cli {
    /// Path to custom configuration file
    #[arg(long, global = true, help = "Path to custom configuration file")]
    config: Option<PathBuf>,
    /// Override maximum retry attempts
    #[arg(short = 'R', long, global = true, help = "Override maximum retry attempts")]
    retries: Option<usize>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Clone, Debug, Default)]
struct CatalogIdArg {
    #[arg(
        long = "id",
        value_name = "ID",
        conflicts_with = "positional_id",
        help = "Catalog ID"
    )]
    flag_id: Option<String>,
    #[arg(value_name = "ID", conflicts_with = "flag_id", help = "Catalog ID")]
    positional_id: Option<String>,
}

impl CatalogIdArg {
    fn provided(&self) -> Option<String> {
        self.flag_id
            .as_deref()
            .and_then(Self::normalize)
            .or_else(|| self.positional_id.as_deref().and_then(Self::normalize))
    }

    fn required(&self, context: &str) -> Result<String> {
        self.provided().ok_or_else(|| {
            anyhow!("{context} is required; pass --id <ID> or supply it as a positional argument")
        })
    }

    fn with_default(&self, default: &str) -> String {
        self.provided().unwrap_or_else(|| default.to_string())
    }

    fn normalize(value: &str) -> Option<String> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Display the currently configured defaults
    Config,
    /// Download a file from the server
    Download {
        #[arg(long, help = "Base host URL (e.g. https://files.example.com)")]
        host: Option<String>,
        /// Catalog ID to download (positional or --id)
        #[command(flatten)]
        target: CatalogIdArg,
        /// Output file (defaults to last path segment)
        #[arg(short = 'O', long)]
        out: Option<String>,
        /// Download directories recursively
        #[arg(short = 'r', long, default_value_t = false)]
        recursive: bool,
        /// Number of parts to split the download into (requires range support)
        #[arg(
            short = 'C',
            long,
            num_args = 0..=1,
            default_missing_value = "16",
            default_value_t = 1,
            value_parser = clap::value_parser!(u8).range(1..=16)
        )]
        connections: u8,
        /// Skip download if local file already exists
        #[arg(long, default_value_t = false, conflicts_with = "dup")]
        skip: bool,
        /// Preserve existing files by writing duplicates with numeric suffix
        #[arg(long, default_value_t = false, conflicts_with = "skip")]
        dup: bool,
    },
    /// Upload a file to the server
    Upload {
        #[arg(long, help = "Base host URL (e.g. https://files.example.com)")]
        host: Option<String>,
        #[arg(value_name = "FILE", value_hint = ValueHint::FilePath)]
        file: String,
        #[arg(long, help = "Upload token (X-Serve-Token)")]
        token: Option<String>,
        #[arg(short = 'p', long, help = "Target directory ID (default root)")]
        parent_id: Option<String>,
        #[arg(
            long,
            default_value_t = false,
            help = "Allow uploads without extension"
        )]
        allow_no_ext: bool,
        #[arg(long, default_value_t = false, help = "Bypass extension whitelist")]
        bypass: bool,
        #[arg(long, default_value_t = false, help = "Use streaming upload")]
        stream: bool,
    },
    /// List directory contents from the server
    List {
        /// Base host URL (e.g. https://files.example.com)
        #[arg(long)]
        host: Option<String>,
        /// Catalog ID to list (positional or --id, defaults to root)
        #[command(flatten)]
        target: CatalogIdArg,
    },
    /// Show URLs and metadata for a specific entry ID
    Info {
        #[arg(long, help = "Base host URL (e.g. https://files.example.com)")]
        host: Option<String>,
        /// Catalog ID to inspect (positional or --id)
        #[command(flatten)]
        target: CatalogIdArg,
    },
    /// Delete a file or directory by catalog ID
    Delete {
        #[arg(long, help = "Base host URL (e.g. https://files.example.com)")]
        host: Option<String>,
        /// Catalog ID to delete (positional or --id)
        #[command(flatten)]
        target: CatalogIdArg,
        #[arg(long, help = "Delete token (X-Serve-Token)")]
        token: Option<String>,
    },
    /// Interactive configuration helper
    Setup,
    /// Display serve-cli version information
    Version,
}

fn main() -> Result<()> {
    cleanup::install_signal_handler();

    let Cli {
        config,
        retries,
        command,
    } = Cli::parse();
    let loaded_config = config::load_config(config.as_deref())?;
    let app_config = loaded_config.data.clone();
    let retry_attempts = resolve_retries(retries, &app_config);

    match command {
        Command::Config => show_config(&loaded_config, config.as_deref()),
        Command::Download {
            host,
            target,
            out,
            recursive,
            connections,
            skip,
            dup,
        } => {
            let resolved_host = resolve_host(host, &app_config);
            let entry_id = target.required("download ID")?;
            let existing_strategy = if skip {
                ExistingFileStrategy::Skip
            } else if dup {
                ExistingFileStrategy::Duplicate
            } else {
                ExistingFileStrategy::Overwrite
            };
            download::download(
                &resolved_host,
                &entry_id,
                out,
                recursive,
                connections.clamp(1, 16),
                existing_strategy,
                retry_attempts,
            )
        }
        Command::Upload {
            host,
            file,
            token,
            parent_id,
            allow_no_ext,
            bypass,
            stream,
        } => {
            let resolved_host = resolve_host(host, &app_config);
            let resolved_token = resolve_token(token, &app_config)?;
            let resolved_parent = resolve_parent_id(parent_id, &app_config);
            let effective_allow = effective_allow_no_ext(allow_no_ext, &app_config);
            upload::upload(
                &resolved_host,
                &file,
                &resolved_token,
                &resolved_parent,
                effective_allow,
                bypass,
                stream,
                retry_attempts,
            )
        }
        Command::List { host, target } => {
            let resolved_host = resolve_host(host, &app_config);
            let entry_id = target.with_default("root");
            list::list(&resolved_host, &entry_id)
        }
        Command::Info { host, target } => {
            let resolved_host = resolve_host(host, &app_config);
            let entry_id = target.required("info ID")?;
            info::show_info(&resolved_host, &entry_id)
        }
        Command::Delete {
            host,
            target,
            token,
        } => {
            let resolved_host = resolve_host(host, &app_config);
            let resolved_token = resolve_token(token, &app_config)?;
            let entry_id = target.required("delete ID")?;
            delete::delete(&resolved_host, &resolved_token, &entry_id)
        }
        Command::Setup => run_setup(config.as_deref(), &app_config),
        Command::Version => {
            println!("{VERSION_SUMMARY}");
            Ok(())
        }
    }
}

fn resolve_host(host_arg: Option<String>, config: &AppConfig) -> String {
    host_arg
        .or_else(|| config.host.clone())
        .unwrap_or_else(|| DEFAULT_HOST.to_string())
}

fn resolve_token(token_arg: Option<String>, config: &AppConfig) -> Result<String> {
    let candidate = token_arg.or_else(|| config.token.clone());
    match candidate {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => Err(anyhow!(
            "upload token is required; pass --token or set it in config"
        )),
    }
}

fn resolve_parent_id(id_arg: Option<String>, config: &AppConfig) -> String {
    id_arg
        .or_else(|| config.upload_parent_id.clone())
        .and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .unwrap_or_else(|| "root".to_string())
}

fn resolve_retries(retry_arg: Option<usize>, config: &AppConfig) -> usize {
    retry_arg
        .filter(|value| *value > 0)
        .or_else(|| {
            config.max_retries.and_then(|value| {
                if value == 0 {
                    None
                } else {
                    Some(value as usize)
                }
            })
        })
        .unwrap_or(DEFAULT_MAX_RETRIES)
}

fn effective_allow_no_ext(flag: bool, config: &AppConfig) -> bool {
    if flag {
        true
    } else {
        config.allow_no_ext.unwrap_or(false)
    }
}

fn run_setup(path_override: Option<&Path>, current: &AppConfig) -> Result<()> {
    let host_default = current.host.as_deref().unwrap_or(DEFAULT_HOST);
    let host = prompt_with_default("Server base URL", host_default)?;
    let token = prompt_optional("Default upload token", current.token.as_deref())?;
    let upload_parent = prompt_optional(
        "Default upload parent ID (blank for root)",
        current.upload_parent_id.as_deref(),
    )?;
    let allow_no_ext = prompt_bool(
        "Allow uploads without extension by default",
        current.allow_no_ext.unwrap_or(false),
    )?;
    let max_retries = prompt_optional_u32(
        "Max retry attempts (blank to keep/default, '-' to clear)",
        current.max_retries,
    )?;

    let mut new_config = AppConfig::default();
    new_config.host = Some(host);
    new_config.token = token;
    new_config.upload_parent_id = upload_parent;
    new_config.allow_no_ext = Some(allow_no_ext);
    new_config.max_retries = max_retries;

    let saved_path = config::save_config(path_override, &new_config)?;
    println!();
    println!("Saved configuration to {}", saved_path.display());
    println!("Tip: pass --config to use a different configuration path.");
    Ok(())
}

fn prompt_with_default(prompt: &str, default: &str) -> Result<String> {
    loop {
        print!("{} [{}]: ", prompt, default);
        io::stdout().flush().context("failed to flush stdout")?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read input")?;
        let value = input.trim();
        if value.is_empty() {
            return Ok(default.to_string());
        }
        return Ok(value.to_string());
    }
}

fn prompt_optional(prompt: &str, current: Option<&str>) -> Result<Option<String>> {
    loop {
        match current {
            Some(existing) if !existing.is_empty() => {
                print!("{} [{}] (blank to keep, '-' to clear): ", prompt, existing)
            }
            Some(_) => print!("{} (blank to keep, '-' to clear): ", prompt),
            None => print!("{} (blank to skip): ", prompt),
        }
        io::stdout().flush().context("failed to flush stdout")?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read input")?;
        let value = input.trim();
        if value.is_empty() {
            return Ok(current.map(|s| s.to_string()));
        }
        if value == "-" {
            return Ok(None);
        }
        return Ok(Some(value.to_string()));
    }
}

fn prompt_bool(prompt: &str, default: bool) -> Result<bool> {
    let hint = if default { "Y/n" } else { "y/N" };
    loop {
        print!("{} [{}]: ", prompt, hint);
        io::stdout().flush().context("failed to flush stdout")?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read input")?;
        let value = input.trim().to_lowercase();
        if value.is_empty() {
            return Ok(default);
        }
        match value.as_str() {
            "y" | "yes" | "true" => return Ok(true),
            "n" | "no" | "false" => return Ok(false),
            _ => println!("Please answer with y/n."),
        }
    }
}

fn prompt_optional_u32(prompt: &str, current: Option<u32>) -> Result<Option<u32>> {
    loop {
        match current {
            Some(existing) => print!("{} [{}] (blank to keep, '-' to clear): ", prompt, existing),
            None => print!("{} (blank to skip): ", prompt),
        }
        io::stdout().flush().context("failed to flush stdout")?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .context("failed to read input")?;
        let value = input.trim();
        if value.is_empty() {
            return Ok(current);
        }
        if value == "-" {
            return Ok(None);
        }
        match value.parse::<u32>() {
            Ok(parsed) if parsed > 0 => return Ok(Some(parsed)),
            _ => println!("Please enter a positive integer."),
        }
    }
}

fn show_config(loaded: &LoadedConfig, override_path: Option<&Path>) -> Result<()> {
    let effective_host = resolve_host(None, &loaded.data);
    let effective_parent = resolve_parent_id(None, &loaded.data);
    let allow = effective_allow_no_ext(false, &loaded.data);

    if let Some(path) = override_path {
        println!("--config arg    : {}", path.display());
    }

    match &loaded.source {
        Some(path) => {
            if loaded.existed {
                println!("Config file     : {} (loaded)", path.display());
            } else {
                println!(
                    "Config file     : {} (missing, using defaults)",
                    path.display()
                );
            }
        }
        None => println!("Config file     : <none> (built-in defaults)"),
    }

    println!("Effective host  : {}", effective_host);
    println!(
        "Default token   : {}",
        loaded
            .data
            .token
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("<not set>")
    );
    println!("Default parent  : {}", effective_parent);
    println!("Allow no ext    : {}", allow);
    let retries = resolve_retries(None, &loaded.data);
    let sleep_secs = total_retry_sleep_seconds(retries);
    println!("Max retries     : {} (max sleep ~{}s)", retries, sleep_secs);
    Ok(())
}
