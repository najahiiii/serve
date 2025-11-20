use std::path::{Path as StdPath, PathBuf};

use axum::body::Body;
use axum::extract::{Multipart, Query, State, multipart::MultipartError};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::{Local, Utc};
use futures_util::StreamExt;
use mime_guess::MimeGuess;
use pathdiff::diff_paths;
use serde::Deserialize;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::catalog::{CatalogCommand, EntryInfo};
use crate::http_utils::{auth_token, build_base_url, client_ip, client_user_agent};
use crate::map_io_error;
use crate::utils::{
    format_modified_time, is_allowed_file, parent_relative_path, secure_filename, unix_timestamp,
};
use crate::{AppError, AppState, NOT_FOUND_MESSAGE, POWERED_BY};

#[derive(Debug, Deserialize)]
pub(crate) struct UploadQuery {
    #[serde(default)]
    pub(crate) dir: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct UploadStreamQuery {
    #[serde(default)]
    pub(crate) dir: Option<String>,
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) allow_no_ext: Option<bool>,
}

pub(crate) async fn handle_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UploadQuery>,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let provided_token = auth_token(&headers);
    if provided_token.as_deref() != Some(state.config.upload_token.as_str()) {
        return Err(AppError::Unauthorized("Unauthorized".to_string()));
    }

    let dir_id = extract_dir_id(&headers, query.dir);
    let (target_dir, resolved_dir_id) = resolve_target_directory(&state, dir_id).await?;

    let mut saved_file = None;

    loop {
        let mut field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(err) => {
                if is_upload_cancelled(&err) {
                    tracing::info!("Upload aborted by client: {}", err);
                    let status = StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST);
                    return Ok(Response::builder()
                        .status(status)
                        .body(Body::empty())
                        .unwrap());
                }
                tracing::error!("Multipart parsing error: {}", err);
                return Err(AppError::BadRequest(
                    "Invalid multipart payload".to_string(),
                ));
            }
        };

        if field.name() != Some("file") {
            continue;
        }

        let file_name = field
            .file_name()
            .map(|name| name.to_string())
            .unwrap_or_default();

        if file_name.is_empty() {
            return Err(AppError::BadRequest(
                "No selected file or file type not allowed".to_string(),
            ));
        }

        let allow_missing_extension = headers
            .get("X-Allow-No-Ext")
            .and_then(|value| value.to_str().ok())
            .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
            .unwrap_or(false);

        let clean_name = StdPath::new(&file_name)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                AppError::BadRequest("No selected file or file type not allowed".to_string())
            })?;

        let has_extension = StdPath::new(clean_name).extension().is_some();
        let extension_allowed = is_allowed_file(clean_name, &state.config.allowed_extensions);

        if !extension_allowed {
            if !(allow_missing_extension && !has_extension) {
                return Err(AppError::BadRequest(
                    "No selected file or file type not allowed".to_string(),
                ));
            }
        }

        let safe_name = secure_filename(clean_name).ok_or_else(|| {
            AppError::BadRequest("No selected file or file type not allowed".to_string())
        })?;

        fs::create_dir_all(&target_dir)
            .await
            .map_err(map_io_error)?;

        let destination_path = target_dir.join(&safe_name);

        let mut output = fs::File::create(&destination_path)
            .await
            .map_err(map_io_error)?;

        let mut total_bytes = 0u64;

        while let Some(chunk) = field.chunk().await.map_err(|err| {
            tracing::error!("Failed to read upload chunk: {}", err);
            AppError::Internal("Internal server error".to_string())
        })? {
            total_bytes += chunk.len() as u64;
            if total_bytes > state.config.max_file_size {
                return Err(AppError::BadRequest("File too large".to_string()));
            }
            output.write_all(&chunk).await.map_err(map_io_error)?;
        }

        let mime_type = field
            .content_type()
            .map(|m| m.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string());

        let relative_path = diff_paths(&destination_path, &*state.canonical_root)
            .unwrap_or_else(|| PathBuf::from(&safe_name));

        let relative_str = relative_path
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");

        let metadata = fs::metadata(&destination_path)
            .await
            .map_err(map_io_error)?;
        let modified_ts = metadata.modified().ok().map(unix_timestamp).unwrap_or(0);
        let entry_info = EntryInfo::new(
            relative_str.clone(),
            safe_name.clone(),
            parent_relative_path(&relative_str),
            false,
            total_bytes,
            mime_type.clone(),
            modified_ts,
        );
        let entry_id = state
            .catalog
            .sync_entry(entry_info)
            .await
            .map_err(|err| AppError::Internal(err.to_string()))?;

        let base_url = build_base_url(&headers);
        let (download_url, list_url) = upload_links(&base_url, &entry_id, &resolved_dir_id);

        let created_date = format_modified_time(Utc::now().with_timezone(&Local));
        saved_file = Some(UploadResponse {
            name: safe_name,
            size_bytes: total_bytes,
            mime_type,
            created_date,
            id: entry_id,
            dir_id: resolved_dir_id.clone(),
            download_url,
            list_url,
            relative_path: relative_str.clone(),
        });

        break;
    }

    let saved = saved_file.ok_or_else(|| AppError::BadRequest("No file to upload".to_string()))?;
    let UploadResponse {
        name,
        size_bytes,
        mime_type,
        created_date,
        id,
        dir_id: response_dir_id,
        download_url,
        list_url,
        relative_path,
    } = saved;

    tracing::info!(
        "[uploading] {} - {} - {} - {}",
        client_ip(&headers),
        name,
        relative_path,
        client_user_agent(&headers)
    );

    let _ = state.catalog_events.try_send(CatalogCommand::RefreshAll);

    let payload = serde_json::json!({
        "status": "success",
        "name": name,
        "id": id,
        "dir_id": response_dir_id,
        "size_bytes": size_bytes,
        "created_date": created_date,
        "mime_type": mime_type,
        "download_url": download_url,
        "list_url": list_url,
        "powered_by": POWERED_BY,
    });

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )
        .body(Body::from(serde_json::to_string_pretty(&payload).unwrap()))
        .unwrap();
    response.headers_mut().insert(
        "X-Upload-Server",
        axum::http::HeaderValue::from_static(POWERED_BY),
    );
    Ok(response)
}

pub(crate) async fn handle_upload_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UploadStreamQuery>,
    body: Body,
) -> Result<Response, AppError> {
    let provided_token = auth_token(&headers);
    if provided_token.as_deref() != Some(state.config.upload_token.as_str()) {
        return Err(AppError::Unauthorized("Unauthorized".to_string()));
    }

    let UploadStreamQuery {
        dir,
        name,
        allow_no_ext,
    } = query;

    let dir_id = extract_dir_id(&headers, dir);
    let (target_dir, resolved_dir_id) = resolve_target_directory(&state, dir_id).await?;

    let mut file_name = name.unwrap_or_default();
    if file_name.is_empty() {
        if let Some(header_name) = headers
            .get("X-Upload-Filename")
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            file_name = header_name.to_string();
        }
    }

    if file_name.is_empty() {
        return Err(AppError::BadRequest("Missing file name".to_string()));
    }

    let allow_missing_extension = headers
        .get("X-Allow-No-Ext")
        .and_then(|value| value.to_str().ok())
        .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or_else(|| allow_no_ext.unwrap_or(false));

    let clean_name = StdPath::new(&file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            AppError::BadRequest("No selected file or file type not allowed".to_string())
        })?;

    let has_extension = StdPath::new(clean_name).extension().is_some();
    let extension_allowed = is_allowed_file(clean_name, &state.config.allowed_extensions);

    if !extension_allowed && !(allow_missing_extension && !has_extension) {
        return Err(AppError::BadRequest(
            "No selected file or file type not allowed".to_string(),
        ));
    }

    let safe_name = secure_filename(clean_name).ok_or_else(|| {
        AppError::BadRequest("No selected file or file type not allowed".to_string())
    })?;

    fs::create_dir_all(&target_dir)
        .await
        .map_err(map_io_error)?;

    let destination_path = target_dir.join(&safe_name);

    if !destination_path.starts_with(&*state.canonical_root) {
        return Err(AppError::BadRequest("Invalid directory path".to_string()));
    }

    let mut output = fs::File::create(&destination_path)
        .await
        .map_err(map_io_error)?;

    let mut total_bytes = 0u64;
    let mut stream = body.into_data_stream();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.map_err(|err| {
            tracing::error!("Failed to read upload stream chunk: {}", err);
            AppError::Internal("Internal server error".to_string())
        })?;

        if chunk.is_empty() {
            continue;
        }

        total_bytes += chunk.len() as u64;
        if total_bytes > state.config.max_file_size {
            return Err(AppError::BadRequest("File too large".to_string()));
        }

        output
            .write_all(chunk.as_ref())
            .await
            .map_err(map_io_error)?;
    }

    output.flush().await.map_err(map_io_error)?;

    let mime_type = MimeGuess::from_path(&safe_name)
        .first_raw()
        .unwrap_or("application/octet-stream")
        .to_string();

    let relative_path = diff_paths(&destination_path, &*state.canonical_root)
        .unwrap_or_else(|| PathBuf::from(&safe_name));

    let relative_str = relative_path
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");

    let metadata = fs::metadata(&destination_path)
        .await
        .map_err(map_io_error)?;
    let modified_ts = metadata.modified().ok().map(unix_timestamp).unwrap_or(0);
    let entry_info = EntryInfo::new(
        relative_str.clone(),
        safe_name.clone(),
        parent_relative_path(&relative_str),
        false,
        total_bytes,
        mime_type.clone(),
        modified_ts,
    );
    let entry_id = state
        .catalog
        .sync_entry(entry_info)
        .await
        .map_err(|err| AppError::Internal(err.to_string()))?;

    let base_url = build_base_url(&headers);
    let (download_url, list_url) = upload_links(&base_url, &entry_id, &resolved_dir_id);

    let created_date = format_modified_time(Utc::now().with_timezone(&Local));
    let saved = UploadResponse {
        name: safe_name,
        size_bytes: total_bytes,
        mime_type,
        created_date,
        id: entry_id,
        dir_id: resolved_dir_id.clone(),
        download_url,
        list_url,
        relative_path: relative_str.clone(),
    };

    tracing::info!(
        "[uploading] {} - {} - {} - {}",
        client_ip(&headers),
        saved.name,
        saved.relative_path,
        client_user_agent(&headers)
    );

    let _ = state.catalog_events.try_send(CatalogCommand::RefreshAll);

    let payload = serde_json::json!({
        "status": "success",
        "name": saved.name,
        "id": saved.id,
        "dir_id": saved.dir_id,
        "size_bytes": saved.size_bytes,
        "created_date": saved.created_date,
        "mime_type": saved.mime_type,
        "download_url": saved.download_url,
        "list_url": saved.list_url,
        "powered_by": POWERED_BY,
    });

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )
        .body(Body::from(serde_json::to_string_pretty(&payload).unwrap()))
        .unwrap();
    response.headers_mut().insert(
        "X-Upload-Server",
        axum::http::HeaderValue::from_static(POWERED_BY),
    );
    Ok(response)
}

fn is_upload_cancelled(err: &MultipartError) -> bool {
    let message = err.to_string();
    message.contains("connection closed")
        || message.contains("Incomplete")
        || message.contains("multipart/form-data")
}

#[derive(Debug)]
struct UploadResponse {
    name: String,
    size_bytes: u64,
    mime_type: String,
    created_date: String,
    id: String,
    dir_id: String,
    download_url: String,
    list_url: String,
    relative_path: String,
}

fn extract_dir_id(headers: &HeaderMap, query_dir: Option<String>) -> Option<String> {
    query_dir
        .and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .or_else(|| {
            headers
                .get("X-Upload-Dir")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|value| value.to_string())
        })
}

async fn resolve_target_directory(
    state: &AppState,
    dir_id: Option<String>,
) -> Result<(PathBuf, String), AppError> {
    let requested = dir_id.unwrap_or_else(|| "root".to_string());
    let trimmed = requested.trim();
    if trimmed.is_empty() {
        return Err(AppError::BadRequest(
            "Directory id cannot be empty".to_string(),
        ));
    }

    if trimmed.eq_ignore_ascii_case("root") {
        return Ok((state.canonical_root.as_ref().clone(), "root".to_string()));
    }

    let entry = state
        .catalog
        .resolve_id(trimmed)
        .await
        .map_err(|err| AppError::Internal(err.to_string()))?
        .ok_or_else(|| AppError::NotFound(NOT_FOUND_MESSAGE.to_string()))?;

    if !entry.is_dir {
        return Err(AppError::BadRequest(
            "Directory id must reference a directory".to_string(),
        ));
    }

    let full_path = if entry.relative_path.is_empty() {
        state.canonical_root.as_ref().clone()
    } else {
        state.canonical_root.join(&entry.relative_path)
    };

    Ok((full_path, trimmed.to_string()))
}

fn upload_links(base_url: &str, file_id: &str, dir_id: &str) -> (String, String) {
    let trimmed = base_url.trim_end_matches('/');
    (
        format!("{}/download?id={}", trimmed, file_id),
        format!("{}/list?id={}", trimmed, dir_id),
    )
}
