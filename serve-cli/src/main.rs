mod cleanup;
mod config;
mod constants;
mod download;
mod http;
mod list;
mod progress;
mod retry;
mod upload;

use crate::config::{AppConfig, LoadedConfig};
use crate::constants::DEFAULT_HOST;
use crate::download::ExistingFileStrategy;
use crate::retry::total_retry_sleep_seconds;
use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
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
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Override maximum retry attempts
    #[arg(long, global = true)]
    retries: Option<usize>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Display the currently configured defaults
    Config,
    /// Download a file from the server
    Download {
        #[arg(long)]
        host: Option<String>,
        /// Remote file path (e.g. /dir/archive.tar)
        #[arg(long)]
        path: String,
        /// Output file (defaults to last path segment)
        #[arg(long)]
        out: Option<String>,
        /// Download directories recursively
        #[arg(long, default_value_t = false)]
        recursive: bool,
        /// Number of parts to split the download into (requires range support)
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=16))]
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
        #[arg(long)]
        host: Option<String>,
        #[arg(long)]
        file: String,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        upload_path: Option<String>,
        #[arg(long, default_value_t = false)]
        allow_no_ext: bool,
        #[arg(long, default_value_t = false)]
        stream: bool,
    },
    /// List directory contents from the server
    List {
        /// Base host URL (e.g. https://files.example.com)
        #[arg(long)]
        host: Option<String>,
        /// Path to list (e.g. / or dir/subdir)
        #[arg(long, default_value = "/")]
        path: String,
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
            path,
            out,
            recursive,
            connections,
            skip,
            dup,
        } => {
            let resolved_host = resolve_host(host, &app_config);
            let existing_strategy = if skip {
                ExistingFileStrategy::Skip
            } else if dup {
                ExistingFileStrategy::Duplicate
            } else {
                ExistingFileStrategy::Overwrite
            };
            download::download(
                &resolved_host,
                &path,
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
            upload_path,
            allow_no_ext,
            stream,
        } => {
            let resolved_host = resolve_host(host, &app_config);
            let resolved_token = resolve_token(token, &app_config)?;
            let resolved_path = resolve_upload_path(upload_path, &app_config);
            let effective_allow = effective_allow_no_ext(allow_no_ext, &app_config);
            upload::upload(
                &resolved_host,
                &file,
                &resolved_token,
                resolved_path.as_deref(),
                effective_allow,
                stream,
                retry_attempts,
            )
        }
        Command::List { host, path } => {
            let resolved_host = resolve_host(host, &app_config);
            list::list(&resolved_host, &path)
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

fn resolve_upload_path(path_arg: Option<String>, config: &AppConfig) -> Option<String> {
    path_arg.or_else(|| config.upload_path.clone())
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
    let upload_path = prompt_optional(
        "Default upload path (blank to skip)",
        current.upload_path.as_deref(),
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
    new_config.upload_path = upload_path;
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
    let effective_path = resolve_upload_path(None, &loaded.data);
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
    println!(
        "Default path    : {}",
        effective_path.as_deref().unwrap_or("<not set>")
    );
    println!("Allow no ext    : {}", allow);
    let retries = resolve_retries(None, &loaded.data);
    let sleep_secs = total_retry_sleep_seconds(retries);
    println!("Max retries     : {} (max sleep ~{}s)", retries, sleep_secs);
    Ok(())
}
