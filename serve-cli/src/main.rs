use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::StatusCode;
use reqwest::Url;
use reqwest::blocking::{Client, Response, multipart};
use reqwest::header::{ACCEPT, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use tabled::{Table, Tabled, settings::Style};

const DEFAULT_HOST: &str = "http://127.0.0.1:3435";
const CLIENT_HEADER_VALUE: &str = "serve-cli";

#[derive(Parser)]
#[command(
    name = "serve-cli",
    version,
    about = "CLI helper for serve-go & serve-rs file servers"
)]
struct Cli {
    /// Path to custom configuration file
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List directory contents from the server
    List {
        /// Base host URL (e.g. https://files.example.com)
        #[arg(long)]
        host: Option<String>,
        /// Path to list (e.g. / or dir/subdir)
        #[arg(long, default_value = "/")]
        path: String,
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
    },
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
    },
    /// Interactive configuration helper
    Setup,
    /// Display the currently configured defaults
    Config,
}

#[derive(Deserialize)]
struct ListResponse {
    path: String,
    entries: Vec<ListEntry>,
    #[serde(default)]
    powered_by: Option<String>,
}

#[derive(Deserialize)]
struct ListEntry {
    name: String,
    size: String,
    #[serde(default)]
    _size_bytes: u64,
    modified: String,
    url: String,
    is_dir: bool,
    #[serde(default)]
    mime_type: String,
}

#[derive(Deserialize, Debug)]
struct UploadResponse {
    status: String,
    name: String,
    size: String,
    path: String,
    view: String,
    download: String,
    #[serde(default)]
    powered_by: String,
}

#[derive(Tabled)]
struct TableEntry {
    #[tabled(rename = "#")]
    index: usize,
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Size")]
    size: String,
    #[tabled(rename = "MIME")]
    mime: String,
    #[tabled(rename = "Modified")]
    modified: String,
    #[tabled(rename = "Type")]
    kind: String,
    #[tabled(rename = "URL")]
    url: String,
}

const CONFIG_FILE_NAME: &str = "serve-cli.toml";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct AppConfig {
    host: Option<String>,
    token: Option<String>,
    upload_path: Option<String>,
    allow_no_ext: Option<bool>,
}

#[derive(Debug, Clone)]
struct LoadedConfig {
    source: Option<PathBuf>,
    existed: bool,
    data: AppConfig,
}

fn main() -> Result<()> {
    let Cli { config, command } = Cli::parse();
    let loaded_config = load_config(config.as_deref())?;
    let app_config = loaded_config.data.clone();

    match command {
        Command::List { host, path } => {
            let resolved_host = resolve_host(host, &app_config);
            list(&resolved_host, &path)
        }
        Command::Upload {
            host,
            file,
            token,
            upload_path,
            allow_no_ext,
        } => {
            let resolved_host = resolve_host(host, &app_config);
            let resolved_token = resolve_token(token, &app_config)?;
            let resolved_path = resolve_upload_path(upload_path, &app_config);
            let effective_allow = effective_allow_no_ext(allow_no_ext, &app_config);
            upload(
                &resolved_host,
                &file,
                &resolved_token,
                resolved_path.as_deref(),
                effective_allow,
            )
        }
        Command::Download {
            host,
            path,
            out,
            recursive,
            connections,
        } => {
            let resolved_host = resolve_host(host, &app_config);
            download(
                &resolved_host,
                &path,
                out,
                recursive,
                connections.clamp(1, 16),
            )
        }
        Command::Setup => run_setup(config.as_deref(), &app_config),
        Command::Config => show_config(&loaded_config, config.as_deref()),
    }
}

fn load_config(path_override: Option<&Path>) -> Result<LoadedConfig> {
    if let Some(path) = path_override {
        let (config, existed) = load_config_from_path(path)?;
        return Ok(LoadedConfig {
            source: Some(path.to_path_buf()),
            existed,
            data: config,
        });
    }

    if let Some(default_path) = default_config_path() {
        let (config, existed) = load_config_from_path(&default_path)?;
        Ok(LoadedConfig {
            source: Some(default_path),
            existed,
            data: config,
        })
    } else {
        Ok(LoadedConfig {
            source: None,
            existed: false,
            data: AppConfig::default(),
        })
    }
}

fn load_config_from_path(path: &Path) -> Result<(AppConfig, bool)> {
    if path.exists() {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let config: AppConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        Ok((config, true))
    } else {
        Ok((AppConfig::default(), false))
    }
}

fn default_config_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "serve").map(|dirs| dirs.config_dir().join(CONFIG_FILE_NAME))
}

fn config_path_for_write(path_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = path_override {
        Ok(path.to_path_buf())
    } else if let Some(path) = default_config_path() {
        Ok(path)
    } else {
        anyhow::bail!("unable to determine configuration directory");
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
        _ => Err(anyhow::anyhow!(
            "upload token is required; pass --token or set it in config"
        )),
    }
}

fn resolve_upload_path(path_arg: Option<String>, config: &AppConfig) -> Option<String> {
    path_arg.or_else(|| config.upload_path.clone())
}

fn effective_allow_no_ext(flag: bool, config: &AppConfig) -> bool {
    if flag {
        true
    } else {
        config.allow_no_ext.unwrap_or(false)
    }
}

fn run_setup(path_override: Option<&Path>, current: &AppConfig) -> Result<()> {
    let config_path = config_path_for_write(path_override)?;
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

    let mut new_config = AppConfig::default();
    new_config.host = Some(host);
    new_config.token = token;
    new_config.upload_path = upload_path;
    new_config.allow_no_ext = Some(allow_no_ext);

    if let Some(parent) = config_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create config directory {}", parent.display())
            })?;
        }
    }

    let toml_string =
        toml::to_string_pretty(&new_config).context("failed to serialize configuration")?;
    fs::write(&config_path, toml_string)
        .with_context(|| format!("failed to write config file {}", config_path.display()))?;

    println!("Saved configuration to {}", config_path.display());
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
            _ => {
                println!("Please answer with y/n.");
            }
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
    Ok(())
}

fn list(host: &str, path: &str) -> Result<()> {
    let client = build_client()?;
    let url = normalize_url(host, path)?;

    let response = client
        .get(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .header(ACCEPT, "application/json")
        .send()
        .with_context(|| format!("request failed for {}", url))?
        .error_for_status()
        .with_context(|| format!("server returned error for {}", url))?;

    let payload: ListResponse = parse_json(response)?;

    if let Some(powered) = payload.powered_by {
        if !powered.is_empty() {
            println!("Server: {}", powered);
        }
    }
    println!("Path: {}", payload.path);

    if payload.entries.is_empty() {
        println!("(empty directory)");
        return Ok(());
    }

    let rows: Vec<TableEntry> = payload
        .entries
        .into_iter()
        .enumerate()
        .map(|(idx, entry)| TableEntry {
            index: idx + 1,
            name: entry.name,
            size: entry.size,
            mime: entry.mime_type,
            modified: entry.modified,
            kind: if entry.is_dir {
                "dir".into()
            } else {
                "file".into()
            },
            url: entry.url,
        })
        .collect();

    let mut table = Table::new(rows);
    table.with(Style::rounded());
    println!("{}", table);

    Ok(())
}

fn upload(
    host: &str,
    file_path: &str,
    token: &str,
    upload_path: Option<&str>,
    allow_no_ext: bool,
) -> Result<()> {
    let client = build_client()?;
    let url = normalize_url(host, "upload")?;

    if !Path::new(file_path).exists() {
        anyhow::bail!("file not found: {}", file_path);
    }

    let file =
        File::open(file_path).with_context(|| format!("failed to open file {}", file_path))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("failed to read metadata for {}", file_path))?;
    let file_size = metadata.len();
    let file_name = Path::new(file_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.bin")
        .to_string();

    let progress = create_progress_bar(Some(file_size), &file_name);
    let reader = ProgressReader::new(file, progress.clone());

    let mut form = multipart::Form::new().part(
        "file",
        multipart::Part::reader_with_length(reader, file_size).file_name(file_name),
    );

    if let Some(path) = upload_path {
        form = form.text("path", path.to_string());
    }

    let mut request = client
        .post(url)
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .header("X-Upload-Token", token)
        .multipart(form);

    if let Some(path) = upload_path {
        request = request.header("X-Upload-Path", path);
    }
    if allow_no_ext {
        request = request.header("X-Allow-No-Ext", "true");
    }

    let response = request.send();
    let response = match response {
        Ok(resp) => resp,
        Err(err) => {
            progress.abandon_with_message("Upload failed");
            return Err(err).context("upload request failed");
        }
    };

    let response = match response.error_for_status() {
        Ok(resp) => resp,
        Err(err) => {
            progress.abandon_with_message("Upload failed");
            return Err(err).context("server returned error for upload");
        }
    };
    progress.finish_with_message("Upload complete");

    let data: UploadResponse = parse_json(response)?;
    if data.status != "success" {
        anyhow::bail!("upload failed: {}", data.status);
    }

    println!("Uploaded: {}", data.name);
    println!("Size: {} bytes", data.size);
    println!("Path: {}", data.path);
    println!("Download: {}", data.download);
    println!("View: {}", data.view);
    if !data.powered_by.is_empty() {
        println!("Server: {}", data.powered_by);
    }

    Ok(())
}

fn download(
    host: &str,
    remote_path: &str,
    out_override: Option<String>,
    recursive: bool,
    connections: u8,
) -> Result<()> {
    let trimmed = remote_path.trim();
    if trimmed.is_empty() {
        anyhow::bail!("remote path is required");
    }

    let client = build_client()?;
    let remote = ensure_leading_slash(trimmed);

    if let Some(listing) = fetch_listing_optional(&client, host, &remote)? {
        if !recursive {
            anyhow::bail!(
                "{} is a directory. Pass --recursive to download it.",
                remote
            );
        }

        let base_local = match out_override {
            Some(path) => Path::new(&path).to_path_buf(),
            None => derive_directory_name(&remote)?,
        };

        let remote_dir = ensure_trailing_slash(&remote);
        download_directory_recursive(
            &client,
            host,
            &remote_dir,
            &base_local,
            listing,
            connections,
        )?;
        println!("Directory saved to {}", base_local.display());
        return Ok(());
    }

    let output_path = match out_override {
        Some(path) => Path::new(&path).to_path_buf(),
        None => derive_file_name(&remote),
    };

    download_file(&client, host, &remote, &output_path, connections)?;
    println!("Saved to {}", output_path.display());
    Ok(())
}

fn build_client() -> Result<Client> {
    Client::builder()
        .user_agent("serve-cli")
        .build()
        .context("failed to build HTTP client")
}

fn normalize_url(base: &str, path: &str) -> Result<Url> {
    let mut url = Url::parse(base).or_else(|_| Url::parse(&format!("http://{}", base)))?;

    let trimmed = path.trim();
    let mut path_to_set = if trimmed.is_empty() || trimmed == "/" {
        String::new()
    } else {
        trimmed.trim_start_matches('/').to_string()
    };

    url.set_path(&path_to_set);
    if trimmed.ends_with('/') && !path_to_set.ends_with('/') && !path_to_set.is_empty() {
        path_to_set.push('/');
        url.set_path(&path_to_set);
    }

    Ok(url)
}

fn parse_json<T: for<'de> Deserialize<'de>>(response: Response) -> Result<T> {
    response
        .json::<T>()
        .context("failed to decode JSON response")
}

fn create_progress_bar(total: Option<u64>, label: &str) -> ProgressBar {
    let formatted = format_label(label);
    if let Some(len) = total {
        let pb = ProgressBar::new(len);
        pb.set_prefix(formatted);
        pb.set_style(
            ProgressStyle::with_template(
                "{prefix} {bar:40.cyan/blue} {bytes}/{total_bytes} ({eta}) {bytes_per_sec}",
            )
            .unwrap()
            .progress_chars("##-"),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        pb
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_prefix(formatted);
        pb.set_style(
            ProgressStyle::with_template("{prefix} {spinner} {bytes} downloaded ({bytes_per_sec})")
                .unwrap()
                .tick_strings(&["-", "\\", "|", "/"]),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        pb
    }
}

fn stream_to_writer(
    response: &mut Response,
    writer: &mut BufWriter<File>,
    pb: &ProgressBar,
) -> Result<u64> {
    let mut buffer = [0u8; 16 * 1024];
    let mut downloaded = 0u64;

    loop {
        let read = response
            .read(&mut buffer)
            .with_context(|| "failed reading response body")?;
        if read == 0 {
            break;
        }
        writer
            .write_all(&buffer[..read])
            .with_context(|| "failed writing to output file")?;
        downloaded += read as u64;

        pb.inc(read as u64);
    }

    writer.flush().context("failed to flush output file")?;
    Ok(downloaded)
}

struct ProgressReader<R> {
    inner: R,
    progress: ProgressBar,
}

impl<R> ProgressReader<R> {
    fn new(inner: R, progress: ProgressBar) -> Self {
        Self { inner, progress }
    }
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let bytes = self.inner.read(buf)?;
        if bytes > 0 {
            self.progress.inc(bytes as u64);
        }
        Ok(bytes)
    }
}

fn format_label(label: &str) -> String {
    const MAX: usize = 25;
    if label.len() <= MAX {
        return label.to_string();
    }
    let mut truncated = label.chars().rev().take(MAX - 3).collect::<String>();
    truncated = truncated.chars().rev().collect();
    format!("...{}", truncated)
}

fn ensure_leading_slash(path: &str) -> String {
    if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    }
}

fn ensure_trailing_slash(path: &str) -> String {
    if path.ends_with('/') {
        path.to_string()
    } else {
        format!("{}/", path.trim_end_matches('/'))
    }
}

fn derive_file_name(remote: &str) -> PathBuf {
    let clean = remote.trim_end_matches('/');
    if let Some(name) = Path::new(clean).file_name().and_then(|s| s.to_str()) {
        PathBuf::from(name)
    } else {
        PathBuf::from("download")
    }
}

fn derive_directory_name(remote: &str) -> Result<PathBuf> {
    let clean = remote.trim_end_matches('/');
    if clean == "/" || clean.is_empty() {
        Ok(PathBuf::from("download"))
    } else if let Some(name) = Path::new(clean).file_name().and_then(|s| s.to_str()) {
        Ok(PathBuf::from(name))
    } else {
        Ok(PathBuf::from("download"))
    }
}

fn fetch_listing_optional(
    client: &Client,
    host: &str,
    remote: &str,
) -> Result<Option<ListResponse>> {
    let listing_path = ensure_trailing_slash(remote);
    let url = normalize_url(host, &listing_path)?;

    let response = client
        .get(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .header(ACCEPT, "application/json")
        .send();

    match response {
        Ok(resp) => {
            if resp.status().is_success() {
                let is_json = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok())
                    .map(|content_type| content_type.contains("application/json"))
                    .unwrap_or(false);
                if !is_json {
                    return Ok(None);
                }
                match resp.json::<ListResponse>() {
                    Ok(data) => Ok(Some(data)),
                    Err(_) => Ok(None),
                }
            } else if resp.status() == StatusCode::NOT_FOUND {
                Ok(None)
            } else {
                Err(anyhow::anyhow!(
                    "listing request failed with status {}",
                    resp.status()
                ))
            }
        }
        Err(err) => {
            if err.status() == Some(StatusCode::NOT_FOUND) {
                Ok(None)
            } else {
                Err(err).context("directory listing request failed")
            }
        }
    }
}

fn download_file(
    client: &Client,
    host: &str,
    remote: &str,
    output: &Path,
    connections: u8,
) -> Result<()> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent directory {}", parent.display())
            })?;
        }
    }

    let url = normalize_url(host, remote)?;
    let probe = probe_file(client, &url)?;

    let label_owned = output
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| output.to_string_lossy().into_owned());

    if let Some(total) = probe.length {
        let mut part_count = usize::from(connections.max(1).min(16));
        if part_count == 0 {
            part_count = 1;
        }
        if !probe.accept_ranges || total == 0 {
            part_count = 1;
        }

        if total == 0 {
            let progress = create_progress_bar(Some(0), &label_owned);
            progress.finish_with_message("Download complete");
            finalize_empty_file(output)?;
            println!("Downloaded 0 bytes from {}", remote);
            return Ok(());
        }

        let mut part_infos = build_part_plan(output, total, part_count)?;
        let mut completed = 0u64;
        for info in &part_infos {
            completed = completed.saturating_add(info.existing_bytes);
        }

        let progress = create_progress_bar(Some(total), &label_owned);
        progress.set_position(completed.min(total));

        for info in &mut part_infos {
            let part_total = info.len();
            if info.existing_bytes >= part_total {
                continue;
            }

            let range_start = info.start + info.existing_bytes;
            let range_end = info.end;

            let mut request = client
                .get(url.clone())
                .header("X-Serve-Client", CLIENT_HEADER_VALUE);

            if range_start > 0 || range_end + 1 != total {
                request = request.header(RANGE, format!("bytes={}-{}", range_start, range_end));
            }

            let mut response = request
                .send()
                .with_context(|| format!("request failed for {}", url))?
                .error_for_status()
                .with_context(|| format!("server returned error for {}", url))?;

            if (range_start > 0 || range_end + 1 != total)
                && response.status() != StatusCode::PARTIAL_CONTENT
            {
                anyhow::bail!("server did not honor range request");
            }

            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .open(&info.path)
                .with_context(|| format!("failed to open temp file {}", info.path.display()))?;
            file.seek(SeekFrom::Start(info.existing_bytes))
                .with_context(|| format!("failed to seek temp file {}", info.path.display()))?;
            let mut writer = BufWriter::new(file);

            let expected = part_total - info.existing_bytes;
            let bytes_written = stream_to_writer(&mut response, &mut writer, &progress)?;
            if bytes_written != expected {
                anyhow::bail!(
                    "download interrupted for part {} (expected {} bytes, got {})",
                    info.index,
                    expected,
                    bytes_written
                );
            }
            info.existing_bytes = part_total;
        }

        progress.finish_with_message("Download complete");
        finalize_parts(output, total, &part_infos)?;
        println!("Downloaded {} bytes from {}", total, remote);
        Ok(())
    } else {
        download_without_length(client, &url, output, &label_owned, probe.accept_ranges)
            .with_context(|| "streaming download failed")?;
        println!("Downloaded file from {}", remote);
        Ok(())
    }
}

struct FileProbe {
    length: Option<u64>,
    accept_ranges: bool,
}

fn probe_file(client: &Client, url: &Url) -> Result<FileProbe> {
    let mut length = None;
    let mut accept_ranges = false;

    let head_response = client
        .head(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .send();

    if let Ok(resp) = head_response {
        if resp.status().is_success() {
            if let Some(value) = resp.headers().get(CONTENT_LENGTH) {
                if let Ok(text) = value.to_str() {
                    if let Ok(parsed) = text.parse::<u64>() {
                        length = Some(parsed);
                    }
                }
            }
            if let Some(value) = resp.headers().get(ACCEPT_RANGES) {
                if let Ok(text) = value.to_str() {
                    if text.eq_ignore_ascii_case("bytes") {
                        accept_ranges = true;
                    }
                }
            }
            return Ok(FileProbe {
                length,
                accept_ranges,
            });
        }
    }

    let request = client
        .get(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .header(RANGE, "bytes=0-0");
    let resp = request.send()?;

    if resp.status() == StatusCode::PARTIAL_CONTENT {
        accept_ranges = true;
    }

    if let Some(value) = resp.headers().get(CONTENT_RANGE) {
        if let Ok(text) = value.to_str() {
            if let Some(pos) = text.rfind('/') {
                if let Ok(total) = text[pos + 1..].parse::<u64>() {
                    length = Some(total);
                }
            }
        }
    } else if let Some(value) = resp.headers().get(CONTENT_LENGTH) {
        if let Ok(text) = value.to_str() {
            if let Ok(parsed) = text.parse::<u64>() {
                length = Some(parsed);
            }
        }
    }

    // Drain body to reuse connection.
    let _ = resp.bytes();

    Ok(FileProbe {
        length,
        accept_ranges,
    })
}

struct PartInfo {
    index: usize,
    start: u64,
    end: u64,
    path: PathBuf,
    existing_bytes: u64,
}

impl PartInfo {
    fn len(&self) -> u64 {
        self.end.saturating_sub(self.start) + 1
    }
}

fn build_part_plan(output: &Path, total: u64, requested_parts: usize) -> Result<Vec<PartInfo>> {
    let parent = output.parent().unwrap_or(Path::new("."));
    let file_name = output
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("output path lacks valid filename"))?;
    let prefix = format!(".{}", file_name);

    let mut parts = Vec::new();
    let chunk_size = (total + requested_parts as u64 - 1) / requested_parts as u64;

    for index in 0..requested_parts {
        let start = index as u64 * chunk_size;
        if start >= total {
            break;
        }
        let end = (start + chunk_size - 1).min(total.saturating_sub(1));
        let path = parent.join(format!("{}.part{:02}.tmp", prefix, index));
        let mut existing_bytes = if path.exists() {
            fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0)
        } else {
            0
        };
        let part_len = end - start + 1;
        if existing_bytes > part_len {
            if let Ok(file) = OpenOptions::new().write(true).open(&path) {
                let _ = file.set_len(part_len);
            }
            existing_bytes = part_len;
        }
        existing_bytes = existing_bytes.min(part_len);
        parts.push(PartInfo {
            index,
            start,
            end,
            path,
            existing_bytes,
        });
    }

    if parts.is_empty() {
        let end = total.saturating_sub(1);
        parts.push(PartInfo {
            index: 0,
            start: 0,
            end,
            path: parent.join(format!("{}.part00.tmp", prefix)),
            existing_bytes: 0,
        });
    }

    Ok(parts)
}

fn finalize_parts(output: &Path, total: u64, parts: &[PartInfo]) -> Result<()> {
    let parent = output.parent().unwrap_or(Path::new("."));
    let file_name = output
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("output path lacks valid filename"))?;
    let temp_path = parent.join(format!(".{}.tmp", file_name));

    let mut final_file = File::create(&temp_path)
        .with_context(|| format!("failed to create temp file {}", temp_path.display()))?;
    for part in parts {
        let mut reader = BufReader::new(
            File::open(&part.path)
                .with_context(|| format!("failed to open part {}", part.path.display()))?,
        );
        io::copy(&mut reader, &mut final_file)
            .with_context(|| format!("failed to merge part {}", part.path.display()))?;
    }
    final_file.flush()?;
    drop(final_file);

    if output.exists() {
        fs::remove_file(output)
            .with_context(|| format!("failed to remove existing file {}", output.display()))?;
    }

    fs::rename(&temp_path, output).with_context(|| {
        format!(
            "failed to move temp file into place for {}",
            output.display()
        )
    })?;

    for part in parts {
        let _ = fs::remove_file(&part.path);
    }

    let meta = fs::metadata(output)
        .with_context(|| format!("failed to stat downloaded file {}", output.display()))?;
    if meta.len() != total {
        anyhow::bail!(
            "merged file size mismatch (expected {} bytes, found {})",
            total,
            meta.len()
        );
    }

    Ok(())
}

fn finalize_empty_file(output: &Path) -> Result<()> {
    let parent = output.parent().unwrap_or(Path::new("."));
    let file_name = output
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("output path lacks valid filename"))?;
    let temp_path = parent.join(format!(".{}.tmp", file_name));
    File::create(&temp_path)?;
    if output.exists() {
        fs::remove_file(output).ok();
    }
    fs::rename(&temp_path, output)?;
    Ok(())
}

fn download_without_length(
    client: &Client,
    url: &Url,
    output: &Path,
    label: &str,
    accept_ranges: bool,
) -> Result<()> {
    let parent = output.parent().unwrap_or(Path::new("."));
    let file_name = output
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("output path lacks valid filename"))?;
    let temp_path = parent.join(format!(".{}.part00.tmp", file_name));

    let mut existing = if temp_path.exists() {
        fs::metadata(&temp_path).map(|meta| meta.len()).unwrap_or(0)
    } else {
        0
    };

    let mut request = client
        .get(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE);
    if accept_ranges && existing > 0 {
        request = request.header(RANGE, format!("bytes={}-", existing));
    }

    let mut response = request.send()?.error_for_status()?;

    if accept_ranges && existing > 0 && response.status() != StatusCode::PARTIAL_CONTENT {
        // Server refused the range; restart download.
        existing = 0;
        let _ = fs::remove_file(&temp_path);
        response = client
            .get(url.clone())
            .header("X-Serve-Client", CLIENT_HEADER_VALUE)
            .send()?;
        response = response.error_for_status()?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&temp_path)?;
    file.seek(SeekFrom::Start(existing))?;
    let mut writer = BufWriter::new(file);

    let progress = create_progress_bar(None, label);
    if existing > 0 {
        progress.inc(existing);
    }
    let _bytes_written = stream_to_writer(&mut response, &mut writer, &progress)?;
    progress.finish_with_message("Download complete");

    drop(writer);
    if output.exists() {
        fs::remove_file(output).ok();
    }
    fs::rename(&temp_path, output)?;
    Ok(())
}

fn download_directory_recursive(
    client: &Client,
    host: &str,
    remote_dir: &str,
    local_dir: &Path,
    listing: ListResponse,
    connections: u8,
) -> Result<()> {
    fs::create_dir_all(local_dir)
        .with_context(|| format!("failed to create directory {}", local_dir.display()))?;

    for entry in listing.entries {
        let mut child_remote = format!("{}{}", remote_dir, entry.name);
        let child_local = local_dir.join(&entry.name);

        if entry.is_dir {
            child_remote = ensure_trailing_slash(&child_remote);
            let child_listing = fetch_listing_optional(client, host, &child_remote)?
                .ok_or_else(|| anyhow::anyhow!("failed to list directory {}", child_remote))?;
            download_directory_recursive(
                client,
                host,
                &child_remote,
                &child_local,
                child_listing,
                connections,
            )?;
        } else {
            download_file(client, host, &child_remote, &child_local, connections)?;
        }
    }

    Ok(())
}
