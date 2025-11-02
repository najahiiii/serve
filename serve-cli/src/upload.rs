use crate::constants::CLIENT_HEADER_VALUE;
use crate::http::{build_client, normalize_url, parse_json};
use crate::progress::{create_progress_bar, finish_progress};
use anyhow::{Context, Result};
use reqwest::blocking::multipart;
use serde::Deserialize;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use indicatif::ProgressBar;

#[derive(Deserialize, Debug)]
pub struct UploadResponse {
    pub status: String,
    pub name: String,
    pub size: String,
    pub path: String,
    pub view: String,
    pub download: String,
    #[serde(default)]
    pub powered_by: String,
}

pub fn upload(
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
    finish_progress(&progress, "Upload complete");

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
