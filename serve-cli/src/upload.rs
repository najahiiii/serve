use crate::constants::CLIENT_HEADER_VALUE;
use crate::http::{build_client, build_endpoint_url, parse_json};
use crate::progress::{create_progress_bar, finish_progress};
use crate::retry::retry;
use anyhow::{Context, Result};
use reqwest::blocking::{Body, Client, RequestBuilder, Response, multipart};
use reqwest::header;
use serde::Deserialize;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use indicatif::ProgressBar;

#[derive(Deserialize, Debug)]
pub struct UploadResponse {
    pub status: String,
    pub id: String,
    pub dir_id: String,
    pub name: String,
    pub size_bytes: u64,
    pub mime_type: String,
    pub created_date: String,
    pub download_url: String,
    pub list_url: String,
    #[serde(default)]
    pub powered_by: String,
}

pub fn upload(
    host: &str,
    file_path: &str,
    token: &str,
    parent_id: &str,
    allow_no_ext: bool,
    stream: bool,
    max_retries: usize,
) -> Result<()> {
    let client = build_client()?;

    if !Path::new(file_path).exists() {
        anyhow::bail!("file not found: {}", file_path);
    }

    let metadata = std::fs::metadata(file_path)
        .with_context(|| format!("failed to read metadata for {}", file_path))?;
    let file_size = metadata.len();
    let file_name = Path::new(file_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("upload.bin")
        .to_string();

    retry("upload", max_retries, || {
        perform_upload_attempt(
            &client,
            host,
            file_path,
            token,
            parent_id,
            allow_no_ext,
            stream,
            file_size,
            &file_name,
        )
    })
}

fn perform_upload_attempt(
    client: &Client,
    host: &str,
    file_path: &str,
    token: &str,
    parent_id: &str,
    allow_no_ext: bool,
    stream: bool,
    file_size: u64,
    file_name: &str,
) -> Result<()> {
    let progress = create_progress_bar(Some(file_size), file_name);

    let response = if stream {
        let mut url = build_endpoint_url(host, "/upload-stream")?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("name", file_name);
            pairs.append_pair("dir", parent_id);
            if allow_no_ext {
                pairs.append_pair("allow_no_ext", "true");
            }
        }

        let file =
            File::open(file_path).with_context(|| format!("failed to open file {}", file_path))?;
        let reader = ProgressReader::new(file, progress.clone());
        let mut request = client
            .put(url)
            .header("X-Serve-Client", CLIENT_HEADER_VALUE)
            .header("X-Upload-Token", token)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::sized(reader, file_size));

        request = request.header("X-Upload-Filename", file_name);
        if allow_no_ext {
            request = request.header("X-Allow-No-Ext", "true");
        }

        execute_request(request, &progress)?
    } else {
        let mut url = build_endpoint_url(host, "/upload")?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs.append_pair("dir", parent_id);
            if allow_no_ext {
                pairs.append_pair("allow_no_ext", "true");
            }
        }
        let file =
            File::open(file_path).with_context(|| format!("failed to open file {}", file_path))?;
        let reader = ProgressReader::new(file, progress.clone());

        let form = multipart::Form::new().part(
            "file",
            multipart::Part::reader_with_length(reader, file_size).file_name(file_name.to_string()),
        );

        let mut request = client
            .post(url)
            .header("X-Serve-Client", CLIENT_HEADER_VALUE)
            .header("X-Upload-Token", token)
            .multipart(form);
        if allow_no_ext {
            request = request.header("X-Allow-No-Ext", "true");
        }

        execute_request(request, &progress)?
    };

    finish_progress(&progress, "Upload complete");

    let data: UploadResponse = parse_json(response)?;
    if data.status != "success" {
        anyhow::bail!("upload failed: {}", data.status);
    }

    println!("Uploaded: {}", data.name);
    println!("Size: {} bytes", data.size_bytes);
    println!("File ID: {}", data.id);
    println!("Parent ID: {}", data.dir_id);
    println!("MIME: {}", data.mime_type);
    println!("Download: {}", data.download_url);
    println!("List: {}", data.list_url);
    println!("Created: {}", data.created_date);
    if !data.powered_by.is_empty() {
        println!("Server: {}", data.powered_by);
    }

    Ok(())
}

fn execute_request(request: RequestBuilder, progress: &ProgressBar) -> Result<Response> {
    let response = match request.send() {
        Ok(resp) => resp,
        Err(err) => {
            progress.abandon_with_message("Upload failed");
            return Err(err).context("upload request failed");
        }
    };

    match response.error_for_status() {
        Ok(resp) => Ok(resp),
        Err(err) => {
            progress.abandon_with_message("Upload failed");
            Err(err).context("server returned error for upload")
        }
    }
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
