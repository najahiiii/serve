use crate::constants::CLIENT_HEADER_VALUE;
use crate::http::{build_client, build_endpoint_url, parse_json};
use anyhow::{Context, Result};
use reqwest::header::ACCEPT;
use serde::Deserialize;
use tabled::{Table, Tabled, settings::Style};

#[derive(Debug, Deserialize)]
pub struct ListResponse {
    pub path: String,
    pub entries: Vec<ListEntry>,
    #[serde(default)]
    pub powered_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListEntry {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    pub name: String,
    pub size: String,
    #[serde(default)]
    pub _size_bytes: u64,
    pub modified: String,
    pub url: String,
    pub is_dir: bool,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub browse_url: Option<String>,
    #[serde(default)]
    pub download_url: Option<String>,
}

#[derive(Tabled)]
struct TableEntry {
    #[tabled(rename = "#")]
    index: usize,
    #[tabled(rename = "ID")]
    id: String,
    #[tabled(rename = "Path")]
    path: String,
    #[tabled(rename = "Name")]
    name: String,
    #[tabled(rename = "Size")]
    size: String,
    #[tabled(rename = "MIME")]
    mime: String,
    #[tabled(rename = "Modified")]
    modified: String,
    #[tabled(rename = "Type")]
    kind: String,
    #[tabled(rename = "URL")]
    url: String,
}

pub fn list(host: &str, id: &str) -> Result<()> {
    let client = build_client()?;
    let mut url = build_endpoint_url(host, "/list")?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.clear();
        pairs.append_pair("id", id);
    }

    let response = client
        .get(url.clone())
        .header("X-Serve-Client", CLIENT_HEADER_VALUE)
        .header(ACCEPT, "application/json")
        .send()
        .with_context(|| format!("request failed for {}", url))?
        .error_for_status()
        .with_context(|| format!("server returned error for {}", url))?;

    let payload: ListResponse = parse_json(response)?;

    if let Some(powered) = payload.powered_by {
        if !powered.is_empty() {
            println!("Server: {}", powered);
        }
    }
    println!("Path: {}", payload.path);

    if payload.entries.is_empty() {
        println!("(empty directory)");
        return Ok(());
    }

    let rows: Vec<TableEntry> = payload
        .entries
        .into_iter()
        .enumerate()
        .map(|(idx, entry)| TableEntry {
            index: idx + 1,
            id: entry.id.clone().unwrap_or_else(|| "-".to_string()),
            path: entry.path.clone().unwrap_or_else(|| "-".to_string()),
            name: entry.name,
            size: entry.size,
            mime: entry.mime_type,
            modified: entry.modified,
            kind: if entry.is_dir {
                "dir".into()
            } else {
                "file".into()
            },
            url: entry
                .download_url
                .clone()
                .or(entry.browse_url.clone())
                .unwrap_or(entry.url),
        })
        .collect();

    let mut table = Table::new(rows);
    table.with(Style::rounded());
    println!("{}", table);

    Ok(())
}
