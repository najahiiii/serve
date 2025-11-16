use std::path::{Path as StdPath, PathBuf};

use axum::body::Body;
use axum::extract::{Multipart, Query, State, multipart::MultipartError};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use chrono::Utc;
use futures_util::StreamExt;
use mime_guess::MimeGuess;
use pathdiff::diff_paths;
use serde::Deserialize;
use tokio::fs;
use tokio::io::AsyncWriteExt;

use crate::catalog::{CatalogCommand, EntryInfo};
use crate::http_utils::{build_base_url, client_ip, client_user_agent};
use crate::map_io_error;
use crate::utils::{
    is_allowed_file, parent_relative_path, resolve_within_root, secure_filename, unix_timestamp,
};
use crate::{AppError, AppState, POWERED_BY};

#[derive(Debug, Deserialize)]
pub(crate) struct UploadQuery {
    #[serde(default)]
    pub(crate) path: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct UploadStreamQuery {
    #[serde(default)]
    pub(crate) path: Option<String>,
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
    let provided_token = headers
        .get("X-Upload-Token")
        .and_then(|value| value.to_str().ok());
    if provided_token != Some(state.config.upload_token.as_str()) {
        return Err(AppError::Unauthorized("Unauthorized".to_string()));
    }

    let mut target_dir_path = query.path.unwrap_or_default();
    if let Some(header_path) = headers
        .get("X-Upload-Path")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_dir_path = header_path.to_string();
    }

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

        if field.name() == Some("path") {
            let text = field.text().await.map_err(|err| {
                tracing::error!("Failed to read path field: {}", err);
                AppError::BadRequest("Invalid directory path".to_string())
            })?;
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                target_dir_path = trimmed.to_string();
            }
            continue;
        }

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

        let target_dir = if target_dir_path.trim().is_empty() {
            state.canonical_root.as_ref().clone()
        } else {
            resolve_within_root(&state.canonical_root, &target_dir_path)
                .ok_or_else(|| AppError::BadRequest("Invalid directory path".to_string()))?
        };

        if !target_dir.starts_with(&*state.canonical_root) {
            return Err(AppError::BadRequest("Invalid directory path".to_string()));
        }

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
        state
            .catalog
            .sync_entry(entry_info)
            .await
            .map_err(|err| AppError::Internal(err.to_string()))?;

        let base_url = build_base_url(&headers);

        saved_file = Some(UploadResponse {
            name: safe_name,
            size: total_bytes.to_string(),
            mime_type,
            created_date: Utc::now().to_rfc3339(),
            path: relative_str.clone(),
            view: format!("{base_url}{relative_str}?view=true"),
            download: format!("{base_url}{relative_str}"),
        });

        break;
    }

    let saved = saved_file.ok_or_else(|| AppError::BadRequest("No file to upload".to_string()))?;
    let UploadResponse {
        name,
        size,
        mime_type,
        created_date,
        path,
        view,
        download,
    } = saved;

    tracing::info!(
        "[uploading] {} - {} - {} - {}",
        client_ip(&headers),
        name,
        path,
        client_user_agent(&headers)
    );

    let _ = state.catalog_events.try_send(CatalogCommand::RefreshAll);

    let payload = serde_json::json!({
        "status": "success",
        "name": name,
        "size": size,
        "created_date": created_date,
        "mime_type": mime_type,
        "path": path,
        "view": view,
        "download": download,
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
    let provided_token = headers
        .get("X-Upload-Token")
        .and_then(|value| value.to_str().ok());
    if provided_token != Some(state.config.upload_token.as_str()) {
        return Err(AppError::Unauthorized("Unauthorized".to_string()));
    }

    let UploadStreamQuery {
        path,
        name,
        allow_no_ext,
    } = query;

    let mut target_dir_path = path.unwrap_or_default();
    if let Some(header_path) = headers
        .get("X-Upload-Path")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        target_dir_path = header_path.to_string();
    }

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

    let target_dir = if target_dir_path.trim().is_empty() {
        state.canonical_root.as_ref().clone()
    } else {
        resolve_within_root(&state.canonical_root, &target_dir_path)
            .ok_or_else(|| AppError::BadRequest("Invalid directory path".to_string()))?
    };

    if !target_dir.starts_with(&*state.canonical_root) {
        return Err(AppError::BadRequest("Invalid directory path".to_string()));
    }

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

    let base_url = build_base_url(&headers);

    let saved = UploadResponse {
        name: safe_name,
        size: total_bytes.to_string(),
        mime_type,
        created_date: Utc::now().to_rfc3339(),
        path: relative_str.clone(),
        view: format!("{base_url}{relative_str}?view=true"),
        download: format!("{base_url}{relative_str}"),
    };

    let metadata = fs::metadata(&destination_path)
        .await
        .map_err(map_io_error)?;
    let modified_ts = metadata.modified().ok().map(unix_timestamp).unwrap_or(0);
    let entry_info = EntryInfo::new(
        relative_str.clone(),
        saved.name.clone(),
        parent_relative_path(&relative_str),
        false,
        total_bytes,
        saved.mime_type.clone(),
        modified_ts,
    );
    state
        .catalog
        .sync_entry(entry_info)
        .await
        .map_err(|err| AppError::Internal(err.to_string()))?;

    tracing::info!(
        "[uploading] {} - {} - {} - {}",
        client_ip(&headers),
        saved.name,
        saved.path,
        client_user_agent(&headers)
    );

    let _ = state.catalog_events.try_send(CatalogCommand::RefreshAll);

    let payload = serde_json::json!({
        "status": "success",
        "name": saved.name,
        "size": saved.size,
        "created_date": saved.created_date,
        "mime_type": saved.mime_type,
        "path": saved.path,
        "view": saved.view,
        "download": saved.download,
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
    size: String,
    mime_type: String,
    created_date: String,
    path: String,
    view: String,
    download: String,
}
