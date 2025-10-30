use chrono::{DateTime, Local};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

const FRAGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`');

pub fn format_size(size_bytes: u64) -> String {
    if size_bytes == 0 {
        return "0 B".to_string();
    }

    let units = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
    let mut size = size_bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < units.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    format!("{size:.2} {}", units[unit_index])
}

pub fn is_allowed_file(filename: &str, allowed_extensions: &HashSet<String>) -> bool {
    Path::new(filename)
        .extension()
        .and_then(OsStr::to_str)
        .map(|ext| allowed_extensions.contains(&ext.to_ascii_lowercase()))
        .unwrap_or(false)
}

pub fn secure_filename(name: &str) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return None;
    }

    let candidate = trimmed
        .chars()
        .filter_map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => Some(ch),
            '-' | '_' | '.' => Some(ch),
            ' ' => Some('_'),
            _ => None,
        })
        .collect::<String>();

    let sanitized = candidate.trim_matches('.');
    if sanitized.is_empty() {
        None
    } else {
        Some(sanitized.to_string())
    }
}

pub fn encode_link(path: &str) -> String {
    let needs_leading_slash = !path.starts_with('/');
    let encoded = utf8_percent_encode(path, FRAGMENT).to_string();
    if needs_leading_slash {
        format!("/{encoded}")
    } else {
        encoded
    }
}

pub fn format_modified_time(time: DateTime<Local>) -> String {
    time.format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn resolve_within_root(root: &Path, requested_path: &str) -> Option<PathBuf> {
    let sanitized = requested_path.trim_start_matches('/');
    let mut depth = 0usize;
    let mut candidate = root.to_path_buf();

    for component in Path::new(sanitized).components() {
        match component {
            Component::CurDir => {}
            Component::Normal(segment) => {
                candidate.push(segment);
                depth += 1;
            }
            Component::ParentDir => {
                if depth == 0 {
                    return None;
                }
                candidate.pop();
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }

    Some(candidate)
}

pub fn is_blacklisted(full_path: &Path, root: &Path, blacklisted: &HashSet<String>) -> bool {
    if let Some(name) = full_path.file_name().and_then(|s| s.to_str()) {
        if blacklisted.contains(name) {
            return true;
        }
    }

    for entry in blacklisted {
        let blocked_path = root.join(entry);
        if full_path.starts_with(&blocked_path) {
            return true;
        }
    }

    false
}
