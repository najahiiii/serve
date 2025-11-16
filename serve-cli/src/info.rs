use crate::constants::CLIENT_HEADER_VALUE;
use crate::http::{build_endpoint_url, parse_json};
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use reqwest::header::ACCEPT;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct InfoResponse {
    id: String,
    name: String,
    path: String,
    mime_type: String,
    is_dir: bool,
    size_bytes: u64,
    size_display: String,
    created: String,
    modified: String,
    parent_id: Option<String>,
    list_url: Option<String>,
    view_url: Option<String>,
    download_url: Option<String>,
}

pub fn show_info(host: &str, id: &str) -> Result<()> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        anyhow::bail!("id value cannot be empty");
    }

    let mut url = build_endpoint_url(host, "/info")?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        pairs.append_pair("id", trimmed);
    }

    let client = Client::builder()
        .user_agent("serve-cli")
        .timeout(None)
        .build()
        .context("failed to build HTTP client")?;

    let response = client
        .get(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .header(ACCEPT, "application/json")
        .send()
        .with_context(|| format!("request failed for {}", url))?
        .error_for_status()
        .with_context(|| format!("server returned error for {}", url))?;

    let data: InfoResponse = parse_json(response)?;
    println!("ID       : {}", data.id);
    println!("Name     : {}", data.name);
    println!("Path     : {}", data.path);
    println!(
        "Type     : {}",
        if data.is_dir { "directory" } else { "file" }
    );
    println!("MIME     : {}", data.mime_type);
    println!(
        "Size     : {} ({} bytes)",
        data.size_display, data.size_bytes
    );
    println!("Created  : {}", data.created);
    println!("Modified : {}", data.modified);
    println!(
        "Parent   : {}",
        data.parent_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("<root>")
    );
    println!("View URL : {}", absolute(host, data.view_url.as_deref()));
    println!(
        "Download : {}",
        absolute(host, data.download_url.as_deref())
    );
    println!("List URL : {}", absolute(host, data.list_url.as_deref()));

    Ok(())
}

fn absolute(host: &str, rel: Option<&str>) -> String {
    match rel {
        Some(path) => {
            let base = host.trim_end_matches('/');
            format!("{}{}", base, path)
        }
        None => "<not available>".to_string(),
    }
}
