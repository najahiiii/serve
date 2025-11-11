use anyhow::{Context, Result};
use reqwest::Url;
use reqwest::blocking::{Client, Response};
use serde::de::DeserializeOwned;

pub fn build_client() -> Result<Client> {
    Client::builder()
        .user_agent("serve-cli")
        .timeout(None)
        .build()
        .context("failed to build HTTP client")
}

pub fn normalize_url(base: &str, path: &str) -> Result<Url> {
    let mut url = Url::parse(base).or_else(|_| Url::parse(&format!("http://{}", base)))?;

    let trimmed = path.trim();
    let mut path_to_set = if trimmed.is_empty() || trimmed == "/" {
        String::new()
    } else {
        trimmed.trim_start_matches('/').to_string()
    };

    url.set_path(&path_to_set);
    if trimmed.ends_with('/') && !path_to_set.ends_with('/') && !path_to_set.is_empty() {
        path_to_set.push('/');
        url.set_path(&path_to_set);
    }

    Ok(url)
}

pub fn parse_json<T: DeserializeOwned>(response: Response) -> Result<T> {
    response
        .json::<T>()
        .context("failed to decode JSON response")
}
