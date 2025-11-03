use axum::http::{HeaderMap, header};

pub(crate) fn host_header(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost")
        .to_string()
}

pub(crate) fn build_base_url(headers: &HeaderMap) -> String {
    let scheme = headers
        .get("X-Forwarded-Proto")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("http");

    let host = host_header(headers);
    format!("{scheme}://{host}/")
}

pub(crate) fn client_ip(headers: &HeaderMap) -> String {
    const CANDIDATES: [&str; 3] = ["x-forwarded-for", "cf-connecting-ip", "x-real-ip"];

    for name in CANDIDATES {
        if let Some(value) = headers
            .get(name)
            .and_then(|header| header.to_str().ok())
            .and_then(|raw| raw.split(',').next())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return value.to_string();
        }
    }

    "unknown".to_string()
}

pub(crate) fn client_user_agent(headers: &HeaderMap) -> String {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|ua| ua.to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
