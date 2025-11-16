use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use chrono::{Datelike, Local};
use html_escape::encode_text;
use mime_guess::MimeGuess;
use serde::Deserialize;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use std::io;
use std::path::PathBuf;

use crate::catalog::{CatalogEntry, EntryInfo};
use crate::http_utils::{build_base_url, client_ip, client_user_agent, host_header};
use crate::map_io_error;
use crate::template;
use crate::utils::{
    encode_link, format_modified_time, format_size, is_blacklisted, parent_relative_path,
    relative_path_string, resolve_within_root, unix_timestamp,
};
use crate::{AppError, AppState, NOT_FOUND_MESSAGE, POWERED_BY, STREAM_BUFFER_BYTES};

#[derive(Debug, Deserialize)]
pub(crate) struct ViewQuery {
    #[serde(default)]
    pub(crate) view: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DownloadIdQuery {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) view: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ListIdQuery {
    pub(crate) id: String,
    #[serde(default)]
    pub(crate) view: Option<bool>,
}

pub(crate) async fn get_root() -> Result<Response, AppError> {
    Response::builder()
        .status(StatusCode::FOUND)
        .header(header::LOCATION, "/list?id=root")
        .body(Body::empty())
        .map_err(|err| AppError::Internal(err.to_string()))
}

async fn serve_path(
    state: AppState,
    headers: HeaderMap,
    requested_path: &str,
    query: ViewQuery,
) -> Result<Response, AppError> {
    let full_path = resolve_within_root(&state.canonical_root, requested_path)
        .ok_or_else(|| AppError::NotFound(NOT_FOUND_MESSAGE.to_string()))?;

    if is_blacklisted(
        &full_path,
        &state.canonical_root,
        &state.config.blacklisted_files,
    ) {
        return Err(AppError::NotFound(NOT_FOUND_MESSAGE.to_string()));
    }

    let metadata = match fs::metadata(&full_path).await {
        Ok(meta) => meta,
        Err(err) => return Err(map_io_error(err)),
    };

    let relative_path = relative_path_string(&state.canonical_root, &full_path)
        .unwrap_or_else(|| requested_path.trim_matches('/').to_string());
    let parent_path = parent_relative_path(&relative_path);
    let name = full_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_string())
        .unwrap_or_else(|| relative_path.clone());
    let mime_type = if metadata.is_dir() {
        "inode/directory".to_string()
    } else {
        MimeGuess::from_path(&full_path)
            .first_raw()
            .unwrap_or("application/octet-stream")
            .to_string()
    };
    let modified_ts = metadata.modified().ok().map(unix_timestamp).unwrap_or(0);
    let size_bytes = if metadata.is_dir() { 0 } else { metadata.len() };
    let entry_info = EntryInfo::new(
        relative_path.clone(),
        name,
        parent_path,
        metadata.is_dir(),
        size_bytes,
        mime_type.clone(),
        modified_ts,
    );
    state
        .catalog
        .sync_entry(entry_info)
        .await
        .map_err(|err| AppError::Internal(err.to_string()))?;

    if metadata.is_dir() {
        render_directory(&state, &headers, requested_path, full_path).await
    } else if metadata.is_file() {
        serve_file(
            &headers,
            requested_path,
            full_path,
            metadata,
            query.view.unwrap_or(false),
        )
        .await
    } else {
        Err(AppError::NotFound(NOT_FOUND_MESSAGE.to_string()))
    }
}

async fn resolve_entry_by_id(state: &AppState, raw_id: &str) -> Result<CatalogEntry, AppError> {
    let id = raw_id.trim();
    if id.is_empty() {
        return Err(AppError::BadRequest("Missing id parameter".to_string()));
    }
    if id.eq_ignore_ascii_case("root") {
        return Ok(CatalogEntry {
            relative_path: String::new(),
            is_dir: true,
        });
    }
    state
        .catalog
        .resolve_id(id)
        .await
        .map_err(|err| AppError::Internal(err.to_string()))?
        .ok_or_else(|| AppError::NotFound(NOT_FOUND_MESSAGE.to_string()))
}

async fn serve_entry_by_relative_path(
    state: AppState,
    headers: HeaderMap,
    relative_path: &str,
    query: ViewQuery,
) -> Result<Response, AppError> {
    let requested_path = relative_path.trim_matches('/');
    serve_path(state, headers, requested_path, query).await
}

pub(crate) async fn download_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DownloadIdQuery>,
) -> Result<Response, AppError> {
    let id = query.id.trim();
    if id.is_empty() {
        return Err(AppError::BadRequest("Missing id parameter".to_string()));
    }

    let entry = resolve_entry_by_id(&state, id).await?;

    if entry.is_dir {
        return Err(AppError::BadRequest(
            "ID refers to a directory; download directories via path".to_string(),
        ));
    }

    serve_entry_by_relative_path(
        state,
        headers,
        &entry.relative_path,
        ViewQuery { view: query.view },
    )
    .await
}

pub(crate) async fn list_by_id(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListIdQuery>,
) -> Result<Response, AppError> {
    let trimmed = query.id.trim().to_string();
    let entry = resolve_entry_by_id(&state, &trimmed).await?;
    if !entry.is_dir {
        let mut location = format!("/download?id={}", trimmed);
        if query.view.unwrap_or(false) {
            location.push_str("&view=true");
        }
        return Response::builder()
            .status(StatusCode::PERMANENT_REDIRECT)
            .header(header::LOCATION, location)
            .body(Body::empty())
            .map_err(|err| AppError::Internal(err.to_string()));
    }
    serve_entry_by_relative_path(
        state,
        headers,
        &entry.relative_path,
        ViewQuery { view: query.view },
    )
    .await
}

async fn render_directory(
    state: &AppState,
    headers: &HeaderMap,
    requested_path: &str,
    directory_path: PathBuf,
) -> Result<Response, AppError> {
    let mut entries = Vec::new();
    let mut read_dir = fs::read_dir(&directory_path).await.map_err(map_io_error)?;

    while let Some(entry) = read_dir.next_entry().await.map_err(map_io_error)? {
        let file_name_os = entry.file_name();
        let file_name = match file_name_os.to_str() {
            Some(name) => name.to_string(),
            None => continue,
        };

        let child_path = entry.path();
        if is_blacklisted(
            &child_path,
            &state.canonical_root,
            &state.config.blacklisted_files,
        ) {
            continue;
        }

        let child_metadata = match entry.metadata().await {
            Ok(metadata) => metadata,
            Err(err) => {
                tracing::error!("Skipping {}: {}", child_path.display(), err);
                continue;
            }
        };

        let modified = match child_metadata.modified() {
            Ok(time) => time,
            Err(err) => {
                tracing::error!("Failed to read mtime for {}: {}", child_path.display(), err);
                continue;
            }
        };
        let is_dir = child_metadata.is_dir();
        let relative_path = match relative_path_string(&state.canonical_root, &child_path) {
            Some(path) => path,
            None => continue,
        };
        let display_name = if is_dir {
            format!("{}/", file_name)
        } else {
            file_name.clone()
        };
        let size_bytes = if is_dir { 0 } else { child_metadata.len() };
        let size_display = if is_dir {
            "-".to_string()
        } else {
            format_size(size_bytes)
        };
        let modified_epoch = unix_timestamp(modified);
        let modified_local: chrono::DateTime<Local> = modified.into();
        let modified_display = format_modified_time(modified_local);
        let mime_type = if is_dir {
            "inode/directory".to_string()
        } else {
            MimeGuess::from_path(&child_path)
                .first_raw()
                .unwrap_or("application/octet-stream")
                .to_string()
        };
        let parent_path = parent_relative_path(&relative_path);
        let entry_info = EntryInfo::new(
            relative_path.clone(),
            file_name.clone(),
            parent_path,
            is_dir,
            size_bytes,
            mime_type.clone(),
            modified_epoch,
        );
        let entry_id = state
            .catalog
            .sync_entry(entry_info)
            .await
            .map_err(|err| AppError::Internal(err.to_string()))?;

        let browse_link = format!("/list?id={}", entry_id);
        let download_link = format!("/download?id={}", entry_id);
        let relative_url = if is_dir {
            browse_link.clone()
        } else {
            download_link.clone()
        };

        entries.push(DirectoryEntry {
            name: file_name,
            display_name,
            relative_url,
            size_bytes,
            size_display,
            modified_display,
            is_dir,
            mime_type,
            id: entry_id,
            relative_path,
            browse_link,
            download_link,
        });
    }

    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    if headers
        .get("X-Serve-Client")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.eq_ignore_ascii_case("serve-cli"))
        .unwrap_or(false)
    {
        let base_url = build_base_url(headers);
        let base_trimmed = base_url.trim_end_matches('/');
        let entries_json: Vec<_> = entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                let browse_absolute = format!("{}{}", base_trimmed, entry.browse_link);
                let download_absolute = format!("{}{}", base_trimmed, entry.download_link);
                let absolute = if entry.is_dir {
                    browse_absolute.clone()
                } else {
                    download_absolute.clone()
                };
                serde_json::json!({
                    "index": idx + 1,
                    "id": entry.id,
                    "name": entry.name,
                    "size": entry.size_display,
                    "size_bytes": entry.size_bytes,
                    "modified": entry.modified_display,
                    "url": absolute,
                    "path": entry.relative_path,
                    "browse_url": browse_absolute,
                    "download_url": download_absolute,
                    "is_dir": entry.is_dir,
                    "mime_type": entry.mime_type,
                })
            })
            .collect();

        let mut normalized_path = if requested_path.is_empty() {
            "/".to_string()
        } else {
            let mut p = format!("/{}", requested_path.trim_start_matches('/'));
            if !p.ends_with('/') {
                p.push('/');
            }
            p
        };
        if normalized_path.is_empty() {
            normalized_path = "/".to_string();
        }

        let payload = serde_json::json!({
            "path": normalized_path,
            "entries": entries_json,
            "powered_by": POWERED_BY,
        });

        let body =
            serde_json::to_vec(&payload).map_err(|err| AppError::Internal(err.to_string()))?;

        return Ok(Response::builder()
            .status(StatusCode::OK)
            .header(
                axum::http::header::CONTENT_TYPE,
                "application/json; charset=utf-8",
            )
            .header("X-Powered-By", POWERED_BY)
            .body(Body::from(body))
            .unwrap());
    }

    let mut rows = String::new();
    if let Some(parent_link) = parent_link(requested_path) {
        rows.push_str(&format!(
            r#"
                <tr>
                    <td class="index"></td>
                    <td class="file-name"><a href="{link}">..</a></td>
                    <td class="file-size"></td>
                    <td class="mime"></td>
                    <td class="date"></td>
                </tr>
            "#,
            link = encode_link(&parent_link)
        ));
    }

    for (idx, entry) in entries.iter().enumerate() {
        rows.push_str(&format!(
            r#"
                <tr>
                    <td class="index">{index}</td>
                    <td class="file-name"><a href="{link}">{display}</a></td>
                    <td class="file-size">{size}</td>
                    <td class="mime">{mime}</td>
                    <td class="date">{modified}</td>
                </tr>
            "#,
            index = idx + 1,
            link = entry.relative_url,
            display = encode_text(&entry.display_name),
            size = entry.size_display,
            mime = encode_text(&entry.mime_type),
            modified = entry.modified_display
        ));
    }

    let host = host_header(headers);
    let directory_label = directory_label(requested_path, &host);
    let current_year = Local::now().year();
    let total_files = entries.len();
    let total_bytes: u64 = entries.iter().map(|entry| entry.size_bytes).sum();
    let disk_usage = format_size(total_bytes);
    let body = template::render_directory_page(
        &directory_label,
        &rows,
        current_year,
        &host,
        &disk_usage,
        total_files,
    );

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(body))
        .unwrap())
}

async fn serve_file(
    headers: &HeaderMap,
    requested_path: &str,
    full_path: PathBuf,
    metadata: std::fs::Metadata,
    view: bool,
) -> Result<Response, AppError> {
    let mut file = fs::File::open(&full_path).await.map_err(map_io_error)?;
    let file_size = metadata.len();
    let mut status = StatusCode::OK;
    let mut content_length = file_size;
    let mut content_range: Option<HeaderValue> = None;

    let body = if let Some(range_value) = headers.get(axum::http::header::RANGE) {
        let range_str = range_value.to_str().unwrap_or("");
        match parse_range_header(range_str, file_size) {
            Ok(Some((start, end))) => {
                status = StatusCode::PARTIAL_CONTENT;
                content_length = end.saturating_sub(start).saturating_add(1);
                file.seek(io::SeekFrom::Start(start))
                    .await
                    .map_err(map_io_error)?;
                let limited = file.take(content_length);
                content_range = Some(
                    HeaderValue::from_str(&format!("bytes {}-{}/{}", start, end, file_size))
                        .unwrap(),
                );
                Body::from_stream(ReaderStream::with_capacity(limited, STREAM_BUFFER_BYTES))
            }
            Ok(None) => Body::from_stream(ReaderStream::with_capacity(file, STREAM_BUFFER_BYTES)),
            Err(_) => {
                let mut response = Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(
                        axum::http::header::CONTENT_RANGE,
                        format!("bytes */{}", file_size),
                    )
                    .body(Body::empty())
                    .unwrap();
                response.headers_mut().insert(
                    axum::http::header::ACCEPT_RANGES,
                    HeaderValue::from_static("bytes"),
                );
                return Ok(response);
            }
        }
    } else {
        Body::from_stream(ReaderStream::with_capacity(file, STREAM_BUFFER_BYTES))
    };

    let mime = MimeGuess::from_path(&full_path)
        .first_or_octet_stream()
        .to_string();

    let disposition_type = if view { "inline" } else { "attachment" };
    let filename = full_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download");

    let mut response = Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, mime)
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!(r#"{disposition_type}; filename="{filename}""#),
        )
        .body(body)
        .unwrap();

    response.headers_mut().insert(
        axum::http::header::CONTENT_LENGTH,
        HeaderValue::from_str(&content_length.to_string()).unwrap(),
    );
    response.headers_mut().insert(
        axum::http::header::ACCEPT_RANGES,
        HeaderValue::from_static("bytes"),
    );
    if let Some(value) = content_range {
        response
            .headers_mut()
            .insert(axum::http::header::CONTENT_RANGE, value);
    }

    let path_display = if requested_path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", requested_path.trim_start_matches('/'))
    };
    tracing::info!(
        "[downloading] {} - {} - {} - {}",
        client_ip(headers),
        filename,
        path_display,
        client_user_agent(headers)
    );

    Ok(response)
}

fn parse_range_header(value: &str, size: u64) -> Result<Option<(u64, u64)>, ()> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(());
    }
    if !trimmed.starts_with("bytes=") {
        return Ok(None);
    }

    let spec = trimmed[6..].trim();
    if spec.is_empty() || spec.contains(',') {
        return Err(());
    }

    let (start_part, end_part) = spec.split_once('-').ok_or(())?;
    let start_part = start_part.trim();
    let end_part = end_part.trim();

    if start_part.is_empty() {
        if end_part.is_empty() {
            return Err(());
        }
        let suffix: u64 = end_part.parse().map_err(|_| ())?;
        if suffix == 0 || size == 0 {
            return Err(());
        }
        let length = suffix.min(size);
        let start = size - length;
        let end = size - 1;
        Ok(Some((start, end)))
    } else {
        let start: u64 = start_part.parse().map_err(|_| ())?;
        let end = if end_part.is_empty() {
            size.checked_sub(1).ok_or(())?
        } else {
            end_part.parse().map_err(|_| ())?
        };
        if start > end || end >= size {
            return Err(());
        }
        Ok(Some((start, end)))
    }
}

fn parent_link(requested_path: &str) -> Option<String> {
    if requested_path.trim().is_empty() {
        return None;
    }

    let mut parts: Vec<&str> = requested_path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();

    if parts.is_empty() {
        return Some("/".to_string());
    }

    parts.pop();
    if parts.is_empty() {
        Some("/".to_string())
    } else {
        Some(format!("/{}", parts.join("/")))
    }
}

fn directory_label(requested_path: &str, host: &str) -> String {
    if requested_path.trim().is_empty() {
        host.to_string()
    } else {
        let parts: Vec<&str> = requested_path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .collect();

        if let Some(last) = parts.last() {
            let prefix = "../".repeat(parts.len().saturating_sub(1));
            format!("{prefix}{last}/")
        } else {
            format!("{requested_path}/")
        }
    }
}

#[derive(Debug)]
struct DirectoryEntry {
    name: String,
    display_name: String,
    relative_url: String,
    size_bytes: u64,
    size_display: String,
    modified_display: String,
    is_dir: bool,
    mime_type: String,
    id: String,
    relative_path: String,
    browse_link: String,
    download_link: String,
}
