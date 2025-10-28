use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::StatusCode;
use reqwest::Url;
use reqwest::blocking::{Client, Response, multipart};
use reqwest::header::ACCEPT;
use serde::Deserialize;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Write};
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
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List directory contents from the server
    List {
        /// Base host URL (e.g. https://files.example.com)
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        /// Path to list (e.g. / or dir/subdir)
        #[arg(long, default_value = "/")]
        path: String,
    },
    /// Upload a file to the server
    Upload {
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        #[arg(long)]
        file: String,
        #[arg(long)]
        token: String,
        #[arg(long, default_value = "")]
        upload_path: String,
        #[arg(long, default_value_t = false)]
        allow_no_ext: bool,
    },
    /// Download a file from the server
    Download {
        #[arg(long, default_value = DEFAULT_HOST)]
        host: String,
        /// Remote file path (e.g. /dir/archive.tar)
        #[arg(long)]
        path: String,
        /// Output file (defaults to last path segment)
        #[arg(long)]
        out: Option<String>,
        /// Download directories recursively
        #[arg(long, default_value_t = false)]
        recursive: bool,
    },
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

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::List { host, path } => list(&host, &path),
        Command::Upload {
            host,
            file,
            token,
            upload_path,
            allow_no_ext,
        } => upload(&host, &file, &token, &upload_path, allow_no_ext),
        Command::Download {
            host,
            path,
            out,
            recursive,
        } => download(&host, &path, out, recursive),
    }
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
    upload_path: &str,
    allow_no_ext: bool,
) -> Result<()> {
    let client = build_client()?;
    let url = normalize_url(host, "upload")?;

    if !Path::new(file_path).exists() {
        anyhow::bail!("file not found: {}", file_path);
    }

    let mut form = multipart::Form::new()
        .file("file", file_path)
        .with_context(|| format!("failed to add file {} to form", file_path))?;

    if !upload_path.is_empty() {
        form = form.text("path", upload_path.to_string());
    }

    let mut request = client
        .post(url)
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .header("X-Upload-Token", token)
        .multipart(form);

    if !upload_path.is_empty() {
        request = request.header("X-Upload-Path", upload_path);
    }
    if allow_no_ext {
        request = request.header("X-Allow-No-Ext", "true");
    }

    let response = request
        .send()
        .context("upload request failed")?
        .error_for_status()
        .context("server returned error for upload")?;

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
        download_directory_recursive(&client, host, &remote_dir, &base_local, listing)?;
        println!("Directory saved to {}", base_local.display());
        return Ok(());
    }

    let output_path = match out_override {
        Some(path) => Path::new(&path).to_path_buf(),
        None => derive_file_name(&remote),
    };

    download_file(&client, host, &remote, &output_path)?;
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

fn content_length(response: &Response) -> Option<u64> {
    response
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|s| s.parse().ok())
}

fn create_progress_bar(total: Option<u64>, label: &str) -> ProgressBar {
    let formatted = format_label(label);
    if let Some(len) = total {
        let pb = ProgressBar::new(len);
        pb.set_prefix(formatted);
        pb.set_style(
            ProgressStyle::with_template(
                "{prefix} {bar:40.cyan/blue} {bytes}/{total_bytes} ({eta})",
            )
            .unwrap()
            .progress_chars("##-"),
        );
        pb
    } else {
        let pb = ProgressBar::new_spinner();
        pb.set_prefix(formatted);
        pb.set_style(
            ProgressStyle::with_template("{prefix} {spinner} {bytes} downloaded")
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

        if pb.length().is_some() {
            pb.set_position(downloaded);
        } else {
            pb.inc(read as u64);
        }
    }

    writer.flush().context("failed to flush output file")?;
    Ok(downloaded)
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
        Ok(PathBuf::from("."))
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

fn download_file(client: &Client, host: &str, remote: &str, output: &Path) -> Result<()> {
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent directory {}", parent.display())
            })?;
        }
    }

    let url = normalize_url(host, remote)?;
    let mut response = client
        .get(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .send()
        .with_context(|| format!("request failed for {}", url))?
        .error_for_status()
        .with_context(|| format!("server returned error for {}", url))?;

    let mut file = BufWriter::new(
        File::create(output)
            .with_context(|| format!("failed to create output file {}", output.display()))?,
    );

    let total = content_length(&response);
    let label_owned = output
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| output.to_string_lossy().into_owned());
    let progress = create_progress_bar(total, &label_owned);

    let bytes_written = stream_to_writer(&mut response, &mut file, &progress)?;
    progress.finish_with_message("Download complete");
    println!("Downloaded {} bytes from {}", bytes_written, remote);
    Ok(())
}

fn download_directory_recursive(
    client: &Client,
    host: &str,
    remote_dir: &str,
    local_dir: &Path,
    listing: ListResponse,
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
            download_directory_recursive(client, host, &child_remote, &child_local, child_listing)?;
        } else {
            download_file(client, host, &child_remote, &child_local)?;
        }
    }

    Ok(())
}
