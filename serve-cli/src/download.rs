use crate::cleanup::{TempCleanupGuard, track_temp_file, untrack_temp_file};
use crate::constants::CLIENT_HEADER_VALUE;
use crate::http::{build_client, normalize_url};
use crate::list::ListResponse;
use crate::progress::{
    self, ActiveConnectionGuard, PARTIAL_STATE_UPDATE_THRESHOLD, create_progress_bar,
    create_progress_bar_with_message, finish_progress,
};
use anyhow::{Context, Result, anyhow};
use indicatif::ProgressBar;
use reqwest::blocking::Client;
use reqwest::header::{ACCEPT, ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, RANGE};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExistingFileStrategy {
    Overwrite,
    Skip,
    Duplicate,
}

#[derive(Debug)]
struct DownloadOutcome {
    path: PathBuf,
    skipped: bool,
}

pub fn download(
    host: &str,
    remote_path: &str,
    out_override: Option<String>,
    recursive: bool,
    connections: u8,
    existing_strategy: ExistingFileStrategy,
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

        let mut base_local = match out_override {
            Some(path) => Path::new(&path).to_path_buf(),
            None => derive_directory_name(&remote)?,
        };

        match existing_strategy {
            ExistingFileStrategy::Duplicate => {
                if base_local.exists() {
                    base_local = next_available_path(&base_local);
                }
            }
            ExistingFileStrategy::Skip => {
                if base_local.exists() {
                    println!(
                        "Skipping download; local directory {} already exists",
                        base_local.display()
                    );
                    return Ok(());
                }
            }
            ExistingFileStrategy::Overwrite => {}
        }

        let remote_dir = ensure_trailing_slash(&remote);
        download_directory_recursive(
            &client,
            host,
            &remote_dir,
            &base_local,
            listing,
            connections,
            existing_strategy,
        )?;
        println!("Directory saved to {}", base_local.display());
        return Ok(());
    }

    let output_path = match out_override {
        Some(path) => Path::new(&path).to_path_buf(),
        None => derive_file_name(&remote),
    };

    let outcome = download_file(
        &client,
        host,
        &remote,
        &output_path,
        connections,
        existing_strategy,
    )?;

    if outcome.skipped {
        println!(
            "Skipped download; keeping existing file at {}",
            outcome.path.display()
        );
    } else {
        println!("Saved to {}", outcome.path.display());
    }
    Ok(())
}

fn finalize_empty_file(output: &Path) -> Result<()> {
    let temp_path = download_temp_path(output)?;
    track_temp_file(temp_path.as_path());
    File::create(&temp_path)?;
    if output.exists() {
        fs::remove_file(output).ok();
    }
    fs::rename(&temp_path, output)?;
    untrack_temp_file(temp_path.as_path());
    clear_partial_state(&temp_path);
    Ok(())
}

fn download_file(
    client: &Client,
    host: &str,
    remote: &str,
    output: &Path,
    connections: u8,
    existing_strategy: ExistingFileStrategy,
) -> Result<DownloadOutcome> {
    let url = normalize_url(host, remote)?;
    let probe = probe_file(client, &url)?;

    let output_path = match existing_strategy {
        ExistingFileStrategy::Duplicate => next_available_path(output),
        _ => output.to_path_buf(),
    };

    if matches!(existing_strategy, ExistingFileStrategy::Skip) && output_path.exists() {
        let message = match fs::metadata(&output_path) {
            Ok(meta) => match probe.length {
                Some(remote_len) => {
                    if meta.len() == remote_len {
                        format!(
                            "Skipping download; {} already exists with matching size ({} bytes)",
                            output_path.display(),
                            remote_len
                        )
                    } else {
                        format!(
                            "Skipping download; {} exists ({} bytes) but remote reports {} bytes. Rerun without --skip to replace it.",
                            output_path.display(),
                            meta.len(),
                            remote_len
                        )
                    }
                }
                None => format!(
                    "Skipping download; {} already exists ({} bytes) and remote size is unknown",
                    output_path.display(),
                    meta.len()
                ),
            },
            Err(err) => format!(
                "Skipping download; {} already exists but metadata could not be read: {}",
                output_path.display(),
                err
            ),
        };
        println!("{}", message);
        return Ok(DownloadOutcome {
            path: output_path,
            skipped: true,
        });
    }

    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).with_context(|| {
                format!("failed to create parent directory {}", parent.display())
            })?;
        }
    }

    let mut cleanup_guard = TempCleanupGuard::new();

    let label_owned = output_path
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| output_path.to_string_lossy().into_owned());

    if matches!(probe.length, Some(0)) {
        finalize_empty_file(&output_path)?;
        println!("Downloaded 0 bytes from {}", remote);
        cleanup_guard.disarm();
        return Ok(DownloadOutcome {
            path: output_path,
            skipped: false,
        });
    }

    let requested_connections = connections.max(1);
    let effective_connections = requested_connections.min(16);
    let multi_supported =
        probe.length.is_some() && probe.accept_ranges && effective_connections > 1;
    if requested_connections > 1 && !multi_supported {
        eprintln!("server does not support multi-connection downloads; using a single connection");
    }

    let downloaded_len = download_to_single_file(
        client,
        &url,
        &output_path,
        &label_owned,
        probe.length,
        probe.accept_ranges,
        effective_connections,
    )
    .with_context(|| "streaming download failed")?;
    cleanup_guard.disarm();

    if let Some(total) = probe.length {
        println!("Downloaded {} bytes from {}", total, remote);
    } else {
        println!("Downloaded {} bytes from {}", downloaded_len, remote);
    }
    Ok(DownloadOutcome {
        path: output_path,
        skipped: false,
    })
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

    let _ = resp.bytes();

    Ok(FileProbe {
        length,
        accept_ranges,
    })
}

fn download_temp_path(output: &Path) -> Result<PathBuf> {
    let parent = output.parent().unwrap_or(Path::new("."));
    let file_name = output
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("output path lacks valid filename"))?;
    Ok(parent.join(format!(".{}.tmp", file_name)))
}

struct RangePart {
    start: u64,
    end: u64,
}

fn build_range_plan(total: u64, requested_parts: usize) -> Vec<RangePart> {
    let mut parts = Vec::new();
    if requested_parts == 0 {
        return parts;
    }
    let chunk_size = (total + requested_parts as u64 - 1) / requested_parts as u64;
    for index in 0..requested_parts {
        let start = index as u64 * chunk_size;
        if start >= total {
            break;
        }
        let end = (start + chunk_size - 1).min(total.saturating_sub(1));
        parts.push(RangePart { start, end });
    }
    parts
}

fn download_with_multiple_connections(
    client: &Client,
    url: &Url,
    temp_path: &Path,
    total: u64,
    progress: &ProgressBar,
    mut state: PartialDownloadState,
) -> Result<PartialDownloadState> {
    state.total = Some(total);
    state.part_count = state.part_count.max(1);
    state.ensure_layout(total);
    let total_connections = state.part_count;
    let active_connections = Arc::new(AtomicUsize::new(0));
    progress::update_connection_message(progress, 0, total_connections);

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(temp_path)
        .with_context(|| format!("failed to open temp file {}", temp_path.display()))?;
    file.set_len(total)
        .with_context(|| format!("failed to size temp file {}", temp_path.display()))?;
    drop(file);

    save_partial_state(temp_path, &state);
    let completed_bytes = state.completed_bytes();
    if completed_bytes > 0 {
        progress.inc(completed_bytes);
    }

    #[derive(Clone)]
    struct PartWork {
        index: usize,
        start: u64,
        end: u64,
        downloaded: u64,
    }

    let state = Arc::new(Mutex::new(state));
    let temp_path_buf = temp_path.to_path_buf();
    let url = url.clone();
    let progress = progress.clone();
    let active_connections_for_threads = active_connections.clone();
    let total_connections_for_threads = total_connections;

    let work_items: Vec<PartWork> = {
        let guard = state.lock().unwrap();
        guard
            .parts
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let len = entry.len();
                let downloaded = entry.downloaded.min(len);
                if downloaded >= len {
                    None
                } else {
                    Some(PartWork {
                        index,
                        start: entry.start,
                        end: entry.end,
                        downloaded,
                    })
                }
            })
            .collect()
    };

    if work_items.is_empty() {
        let final_state = match Arc::try_unwrap(state) {
            Ok(mutex) => mutex
                .into_inner()
                .unwrap_or_else(|_| PartialDownloadState::new(Some(total), 1)),
            Err(arc) => arc.lock().unwrap().clone(),
        };
        return Ok(final_state);
    }

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(work_items.len());

        for work in work_items {
            let client_ref = client.clone();
            let url = url.clone();
            let temp_path = temp_path_buf.clone();
            let progress = progress.clone();
            let state = state.clone();
            let connection_counter = active_connections_for_threads.clone();
            let total_connections = total_connections_for_threads;

            handles.push(scope.spawn(move || -> Result<()> {
                let PartWork {
                    index,
                    start,
                    end,
                    downloaded,
                } = work;

                let _connection_guard =
                    ActiveConnectionGuard::new(connection_counter, &progress, total_connections);

                let range_start = start.saturating_add(downloaded);
                let request = client_ref
                    .get(url.clone())
                    .header("X-Serve-Client", CLIENT_HEADER_VALUE)
                    .header(RANGE, format!("bytes={}-{}", range_start, end));

                let mut response = request
                    .send()
                    .with_context(|| format!("request failed for part {}", index))?
                    .error_for_status()
                    .with_context(|| format!("server returned error for part {}", index))?;

                if response.status() != StatusCode::PARTIAL_CONTENT {
                    return Err(anyhow!(
                        "server did not honor range request for part {}",
                        index
                    ));
                }

                let part_length = end.saturating_sub(start).saturating_add(1);
                let mut local_downloaded = downloaded.min(part_length);
                let mut remaining = part_length.saturating_sub(local_downloaded);
                let mut buffer = vec![0u8; 16 * 1024];
                let mut last_persisted = local_downloaded;

                let mut file = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .open(&temp_path)
                    .with_context(|| format!("failed to open temp file {}", temp_path.display()))?;
                file.seek(SeekFrom::Start(start.saturating_add(local_downloaded)))
                    .with_context(|| format!("failed to seek temp file {}", temp_path.display()))?;
                let mut writer = BufWriter::new(file);

                while remaining > 0 {
                    let to_read = remaining.min(buffer.len() as u64) as usize;
                    let read = response
                        .read(&mut buffer[..to_read])
                        .with_context(|| format!("failed reading response for part {}", index))?;
                    if read == 0 {
                        break;
                    }

                    writer
                        .write_all(&buffer[..read])
                        .with_context(|| format!("failed writing part {} to temp file", index))?;
                    remaining -= read as u64;
                    local_downloaded += read as u64;
                    progress.inc(read as u64);

                    if local_downloaded.saturating_sub(last_persisted)
                        >= PARTIAL_STATE_UPDATE_THRESHOLD
                    {
                        let mut guard = state.lock().unwrap();
                        guard.set_downloaded(index, local_downloaded);
                        save_partial_state(&temp_path, &guard);
                        last_persisted = local_downloaded;
                    }
                }

                writer.flush()?;

                if local_downloaded > last_persisted {
                    let mut guard = state.lock().unwrap();
                    guard.set_downloaded(index, local_downloaded);
                    save_partial_state(&temp_path, &guard);
                }

                if remaining > 0 {
                    return Err(anyhow!(
                        "download interrupted for part {} ({} bytes remaining)",
                        index,
                        remaining
                    ));
                }

                Ok(())
            }));
        }

        for handle in handles {
            match handle.join() {
                Ok(result) => result?,
                Err(panic) => {
                    if let Ok(message) = panic.downcast::<String>() {
                        return Err(anyhow!("download worker panicked: {}", *message));
                    } else {
                        return Err(anyhow!("download worker panicked"));
                    }
                }
            }
        }

        Ok(())
    })?;

    progress::update_connection_message(
        &progress,
        active_connections.load(Ordering::SeqCst),
        total_connections,
    );

    let final_state = match Arc::try_unwrap(state) {
        Ok(mutex) => mutex
            .into_inner()
            .unwrap_or_else(|_| PartialDownloadState::new(Some(total), 1)),
        Err(arc) => arc.lock().unwrap().clone(),
    };

    Ok(final_state)
}

fn download_to_single_file(
    client: &Client,
    url: &Url,
    output: &Path,
    label: &str,
    total: Option<u64>,
    accept_ranges: bool,
    connections: u8,
) -> Result<u64> {
    let temp_path = download_temp_path(output)?;
    track_temp_file(temp_path.as_path());

    let mut existing = if temp_path.exists() {
        fs::metadata(&temp_path).map(|meta| meta.len()).unwrap_or(0)
    } else {
        0
    };
    let mut partial_state = load_partial_state(&temp_path);

    if let Some(state) = &partial_state {
        if let (Some(saved_total), Some(current_total)) = (state.total, total) {
            if saved_total != current_total {
                eprintln!(
                    "existing partial download has mismatched size; restarting {}",
                    output.display()
                );
                let _ = fs::remove_file(&temp_path);
                clear_partial_state(&temp_path);
                partial_state = None;
                existing = 0;
            }
        }
    }

    if let Some(total) = total {
        if existing >= total && total > 0 {
            match partial_state.as_ref() {
                Some(state) if state.is_complete() => {
                    return finalize_temp_file(&temp_path, output, Some(total));
                }
                Some(_) => {}
                None => {
                    return finalize_temp_file(&temp_path, output, Some(total));
                }
            }
        } else if existing > total {
            if let Ok(file) = OpenOptions::new().write(true).open(&temp_path) {
                let _ = file.set_len(total);
            }
            existing = total;
        }
    }

    if existing > 0 && !accept_ranges {
        existing = 0;
        let _ = fs::remove_file(&temp_path);
        clear_partial_state(&temp_path);
        partial_state = None;
    }

    let part_count = connections.max(1) as usize;

    if let Some(total) = total {
        if accept_ranges && part_count > 1 {
            let mut state =
                partial_state.unwrap_or_else(|| PartialDownloadState::new(Some(total), part_count));
            if state.part_count != part_count {
                eprintln!(
                    "resuming multi-connection download with {} connections to match existing partial data",
                    state.part_count
                );
            }
            let total_connections = state.part_count.max(1);
            state.total = Some(total);
            state.ensure_layout(total);
            let progress = create_progress_bar_with_message(
                Some(total),
                label,
                progress::connection_status_message(0, total_connections),
            );
            let _final_state = download_with_multiple_connections(
                client, url, &temp_path, total, &progress, state,
            )?;
            finish_progress(&progress, "Download complete");
            return finalize_temp_file(&temp_path, output, Some(total));
        } else if partial_state.is_some() {
            clear_partial_state(&temp_path);
            partial_state = None;
        }
    }

    drop(partial_state);

    let mut request = client
        .get(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE);
    if accept_ranges && existing > 0 {
        request = request.header(RANGE, format!("bytes={}-", existing));
    }

    let mut response = request.send()?.error_for_status()?;

    if accept_ranges && existing > 0 && response.status() != StatusCode::PARTIAL_CONTENT {
        existing = 0;
        let _ = fs::remove_file(&temp_path);
        clear_partial_state(&temp_path);
        response = client
            .get(url.clone())
            .header("X-Serve-Client", CLIENT_HEADER_VALUE)
            .send()?
            .error_for_status()?;
    }

    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&temp_path)
        .with_context(|| format!("failed to open temp file {}", temp_path.display()))?;
    file.seek(SeekFrom::Start(existing))
        .with_context(|| format!("failed to seek temp file {}", temp_path.display()))?;
    let mut writer = BufWriter::new(file);

    let progress = create_progress_bar(total, label);
    if existing > 0 {
        progress.inc(existing);
    }
    let _bytes_written = stream_to_writer(&mut response, &mut writer, &progress)?;
    finish_progress(&progress, "Download complete");

    drop(writer);

    finalize_temp_file(&temp_path, output, total)
}

fn stream_to_writer(
    response: &mut reqwest::blocking::Response,
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

fn finalize_temp_file(temp_path: &Path, output: &Path, total: Option<u64>) -> Result<u64> {
    if output.exists() {
        fs::remove_file(output)
            .with_context(|| format!("failed to remove existing file {}", output.display()))?;
    }
    fs::rename(temp_path, output).with_context(|| {
        format!(
            "failed to move temp file into place for {}",
            output.display()
        )
    })?;
    untrack_temp_file(temp_path);
    clear_partial_state(temp_path);

    let final_meta = fs::metadata(output)
        .with_context(|| format!("failed to stat downloaded file {}", output.display()))?;
    if let Some(expected) = total {
        if final_meta.len() != expected {
            anyhow::bail!(
                "downloaded file size mismatch (expected {} bytes, found {})",
                expected,
                final_meta.len()
            );
        }
    }

    Ok(final_meta.len())
}

fn download_directory_recursive(
    client: &Client,
    host: &str,
    remote_dir: &str,
    local_dir: &Path,
    listing: ListResponse,
    connections: u8,
    existing_strategy: ExistingFileStrategy,
) -> Result<()> {
    fs::create_dir_all(local_dir)
        .with_context(|| format!("failed to create directory {}", local_dir.display()))?;

    for entry in listing.entries {
        let mut child_remote = format!("{}{}", remote_dir, entry.name);
        let child_local = local_dir.join(&entry.name);

        if entry.is_dir {
            let mut target_local = child_local;
            if matches!(existing_strategy, ExistingFileStrategy::Duplicate) && target_local.exists()
            {
                target_local = next_available_path(&target_local);
            }

            if matches!(existing_strategy, ExistingFileStrategy::Skip) && target_local.exists() {
                println!(
                    "Skipping download of directory {}; already exists",
                    target_local.display()
                );
                continue;
            }

            child_remote = ensure_trailing_slash(&child_remote);
            let child_listing = fetch_listing_optional(client, host, &child_remote)?
                .ok_or_else(|| anyhow::anyhow!("failed to list directory {}", child_remote))?;
            download_directory_recursive(
                client,
                host,
                &child_remote,
                &target_local,
                child_listing,
                connections,
                existing_strategy,
            )?;
        } else {
            download_file(
                client,
                host,
                &child_remote,
                &child_local,
                connections,
                existing_strategy,
            )?;
        }
    }

    Ok(())
}

fn fetch_listing_optional(
    client: &Client,
    host: &str,
    remote: &str,
) -> Result<Option<ListResponse>> {
    #[derive(Debug)]
    enum ListingProbe {
        Listing(ListResponse),
        NotFound,
        NotDirectory,
    }

    fn try_fetch(client: &Client, host: &str, path: &str) -> Result<ListingProbe> {
        let url = normalize_url(host, path)?;
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
                        return Ok(ListingProbe::NotDirectory);
                    }
                    match resp.json::<ListResponse>() {
                        Ok(data) => Ok(ListingProbe::Listing(data)),
                        Err(_) => Ok(ListingProbe::NotDirectory),
                    }
                } else if resp.status() == StatusCode::NOT_FOUND {
                    Ok(ListingProbe::NotFound)
                } else {
                    Err(anyhow::anyhow!(
                        "listing request failed with status {}",
                        resp.status()
                    ))
                }
            }
            Err(err) => {
                if err.status() == Some(StatusCode::NOT_FOUND) {
                    Ok(ListingProbe::NotFound)
                } else {
                    Err(err).context("directory listing request failed")
                }
            }
        }
    }

    match try_fetch(client, host, remote)? {
        ListingProbe::Listing(listing) => Ok(Some(listing)),
        ListingProbe::NotDirectory | ListingProbe::NotFound => Ok(None),
    }
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

fn next_available_path(base: &Path) -> PathBuf {
    if !base.exists() {
        return base.to_path_buf();
    }

    let mut index = 1usize;
    loop {
        let candidate = path_with_suffix(base, index);
        if !candidate.exists() {
            return candidate;
        }
        index = index
            .checked_add(1)
            .expect("exhausted duplicate suffixes while searching for unique path");
    }
}

fn path_with_suffix(base: &Path, index: usize) -> PathBuf {
    let parent = base.parent().unwrap_or_else(|| Path::new(""));
    let file_name = base
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    let (stem, ext) = if file_name.is_empty() {
        ("download".to_string(), None)
    } else if let Some(pos) = file_name.rfind('.') {
        let (stem, ext) = file_name.split_at(pos);
        if stem.is_empty() {
            (file_name, None)
        } else if ext.len() > 1 {
            (stem.to_string(), Some(ext[1..].to_string()))
        } else {
            (stem.to_string(), None)
        }
    } else {
        (file_name, None)
    };

    let mut candidate = format!("{}-{}", stem, index);
    if let Some(ext) = ext {
        candidate.push('.');
        candidate.push_str(&ext);
    }

    if parent.as_os_str().is_empty() {
        PathBuf::from(candidate)
    } else {
        parent.join(candidate)
    }
}

fn load_partial_state(temp_path: &Path) -> Option<PartialDownloadState> {
    let path = partial_state_path(temp_path);
    let data = fs::read(&path).ok()?;
    match serde_json::from_slice::<PartialDownloadState>(&data) {
        Ok(mut state) => {
            state.part_count = state.part_count.max(1);
            Some(state)
        }
        Err(err) => {
            eprintln!(
                "warning: failed to parse partial download state {}: {}",
                path.display(),
                err
            );
            None
        }
    }
}

fn save_partial_state(temp_path: &Path, state: &PartialDownloadState) {
    let path = partial_state_path(temp_path);
    let tmp = partial_state_tmp_path(temp_path);
    match serde_json::to_vec(state) {
        Ok(data) => {
            if let Err(err) = fs::write(&tmp, &data) {
                eprintln!(
                    "warning: failed to persist partial download state {}: {}",
                    tmp.display(),
                    err
                );
                return;
            }
            if let Err(err) = fs::rename(&tmp, &path) {
                let _ = fs::remove_file(&tmp);
                eprintln!(
                    "warning: failed to finalize partial download state {}: {}",
                    path.display(),
                    err
                );
            }
        }
        Err(err) => {
            eprintln!("warning: failed to serialize partial download state: {err}");
        }
    }
}

fn clear_partial_state(temp_path: &Path) {
    let path = partial_state_path(temp_path);
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    let tmp = partial_state_tmp_path(temp_path);
    if tmp.exists() {
        let _ = fs::remove_file(tmp);
    }
}

fn partial_state_path(temp_path: &Path) -> PathBuf {
    temp_path.with_extension("tmp.state")
}

fn partial_state_tmp_path(temp_path: &Path) -> PathBuf {
    temp_path.with_extension("tmp.state.tmp")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PartialDownloadState {
    total: Option<u64>,
    part_count: usize,
    parts: Vec<PartProgress>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PartProgress {
    start: u64,
    end: u64,
    downloaded: u64,
}

impl PartProgress {
    fn len(&self) -> u64 {
        self.end.saturating_sub(self.start).saturating_add(1)
    }
}

impl PartialDownloadState {
    fn new(total: Option<u64>, part_count: usize) -> Self {
        let count = part_count.max(1);
        let mut state = Self {
            total,
            part_count: count,
            parts: Vec::new(),
        };
        if let Some(total) = total {
            state.rebuild_parts(total);
        } else {
            state.parts = vec![PartProgress {
                start: 0,
                end: 0,
                downloaded: 0,
            }];
        }
        state
    }

    fn rebuild_parts(&mut self, total: u64) {
        let plan = build_range_plan(total, self.part_count);
        self.parts = plan
            .into_iter()
            .map(|part| PartProgress {
                start: part.start,
                end: part.end,
                downloaded: 0,
            })
            .collect();
    }

    fn ensure_layout(&mut self, total: u64) {
        self.part_count = self.part_count.max(1);
        if self.parts.len() != self.part_count {
            self.rebuild_parts(total);
            return;
        }
        let plan = build_range_plan(total, self.part_count);
        for (entry, part) in self.parts.iter_mut().zip(plan.into_iter()) {
            entry.start = part.start;
            entry.end = part.end;
            let len = entry.len();
            if entry.downloaded > len {
                entry.downloaded = len;
            }
        }
    }

    fn is_complete(&self) -> bool {
        self.parts
            .iter()
            .all(|entry| entry.downloaded >= entry.len())
    }

    fn completed_bytes(&self) -> u64 {
        self.parts
            .iter()
            .map(|entry| entry.downloaded.min(entry.len()))
            .sum()
    }

    fn set_downloaded(&mut self, index: usize, downloaded: u64) {
        if let Some(entry) = self.parts.get_mut(index) {
            let len = entry.len();
            entry.downloaded = downloaded.min(len);
        }
    }
}
