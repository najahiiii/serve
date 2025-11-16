use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_FILE_NAME: &str = "serve-cli.toml";

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AppConfig {
    pub host: Option<String>,
    pub token: Option<String>,
    #[serde(default, alias = "upload_path")]
    pub upload_parent_id: Option<String>,
    pub allow_no_ext: Option<bool>,
    pub max_retries: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub source: Option<PathBuf>,
    pub existed: bool,
    pub data: AppConfig,
}

pub fn load_config(path_override: Option<&Path>) -> Result<LoadedConfig> {
    if let Some(path) = path_override {
        let (config, existed) = load_config_from_path(path)?;
        return Ok(LoadedConfig {
            source: Some(path.to_path_buf()),
            existed,
            data: config,
        });
    }

    if let Some(default_path) = default_config_path() {
        let (config, existed) = load_config_from_path(&default_path)?;
        Ok(LoadedConfig {
            source: Some(default_path),
            existed,
            data: config,
        })
    } else {
        Ok(LoadedConfig {
            source: None,
            existed: false,
            data: AppConfig::default(),
        })
    }
}

pub fn config_path_for_write(path_override: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = path_override {
        Ok(path.to_path_buf())
    } else if let Some(path) = default_config_path() {
        Ok(path)
    } else {
        anyhow::bail!("unable to determine configuration directory");
    }
}

pub fn save_config(path_override: Option<&Path>, config: &AppConfig) -> Result<PathBuf> {
    let path = config_path_for_write(path_override)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create configuration directory {}",
                parent.display()
            )
        })?;
    }
    let content = toml::to_string_pretty(config).context("failed to serialize configuration")?;
    fs::write(&path, content)
        .with_context(|| format!("failed to write configuration to {}", path.display()))?;
    Ok(path)
}

fn load_config_from_path(path: &Path) -> Result<(AppConfig, bool)> {
    if path.exists() {
        let content = fs::read_to_string(path)
            .with_context(|| format!("failed to read config file {}", path.display()))?;
        let config: AppConfig = toml::from_str(&content)
            .with_context(|| format!("failed to parse config file {}", path.display()))?;
        Ok((config, true))
    } else {
        Ok((AppConfig::default(), false))
    }
}

fn default_config_path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "serve").map(|dirs| dirs.config_dir().join(CONFIG_FILE_NAME))
}
