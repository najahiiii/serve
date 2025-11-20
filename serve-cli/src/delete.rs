use crate::constants::CLIENT_HEADER_VALUE;
use crate::http::{build_client, build_endpoint_url, parse_json};
use anyhow::{Context, Result};
use reqwest::header::ACCEPT;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct DeletePayload {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub is_dir: bool,
    pub status: String,
}

pub fn delete(host: &str, token: &str, id: &str) -> Result<()> {
    let client = build_client()?;
    let mut url = build_endpoint_url(host, "/delete")?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        pairs.append_pair("id", id);
    }

    let response = client
        .delete(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .header("X-Serve-Token", token)
        .header(ACCEPT, "application/json")
        .send()
        .with_context(|| format!("request failed for {}", url))?
        .error_for_status()
        .with_context(|| format!("server returned error for {}", url))?;

    let payload: DeletePayload = parse_json(response)?;
    let kind = if payload.is_dir { "directory" } else { "file" };

    println!("Deleted {} ({})", payload.path, kind);
    println!("Status: {}", payload.status);
    println!("ID: {}", payload.id);

    Ok(())
}
