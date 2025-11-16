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

pub fn build_endpoint_url(base: &str, endpoint: &str) -> Result<Url> {
    let mut url = Url::parse(base).or_else(|_| Url::parse(&format!("http://{}", base)))?;
    let cleaned = endpoint.trim_start_matches('/');
    url.set_path(cleaned);
    Ok(url)
}

pub fn parse_json<T: DeserializeOwned>(response: Response) -> Result<T> {
    response
        .json::<T>()
        .context("failed to decode JSON response")
}
