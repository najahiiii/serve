use crate::constants::CLIENT_HEADER_VALUE;
use crate::http::{build_client, build_endpoint_url, parse_json};
use crate::progress::{create_progress_bar, finish_progress};
use crate::retry::retry;
use anyhow::anyhow;
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
    bypass_ext: bool,
    stream: bool,
    max_retries: usize,
) -> Result<()> {
    let client = build_client()?;

    let path = Path::new(file_path);
    if !path.exists() {
        anyhow::bail!("file not found: {}", file_path);
    }

    let metadata = std::fs::metadata(file_path)
        .with_context(|| format!("failed to read metadata for {}", file_path))?;
    if metadata.is_dir() {
        return Err(anyhow!("cannot upload directories; supply a file path"));
    }
    let file_size = metadata.len();
    let file_name = path
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
            bypass_ext,
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
    bypass_ext: bool,
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
            .header("X-Serve-Token", token)
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .body(Body::sized(reader, file_size));

        request = request.header("X-Upload-Filename", file_name);
        if allow_no_ext {
            request = request.header("X-Allow-No-Ext", "true");
        }
        if bypass_ext {
            request = request.header("X-Allow-All-Ext", "true");
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
            .header("X-Serve-Token", token)
            .multipart(form);
        if allow_no_ext {
            request = request.header("X-Allow-No-Ext", "true");
        }
        if bypass_ext {
            request = request.header("X-Allow-All-Ext", "true");
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
            progress.finish_and_clear();
            return Err(err).context("upload request failed");
        }
    };

    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }

    let body = response
        .text()
        .unwrap_or_else(|err| format!("failed to read error body: {err}"));
    progress.finish_and_clear();
    let detail = body.trim();
    if detail.is_empty() {
        Err(anyhow!(
            "server returned error for upload (status {status})"
        ))
    } else {
        Err(anyhow!(
            "server returned error for upload (status {status}): {detail}"
        ))
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
