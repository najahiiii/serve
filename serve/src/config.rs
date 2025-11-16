use serde::Deserialize;
use std::collections::HashSet;
use std::env;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Application configuration values.
#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub upload_token: String,
    pub max_file_size: u64,
    pub blacklisted_files: HashSet<String>,
    pub allowed_extensions: HashSet<String>,
    pub root_override: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub root_source: RootSource,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootSource {
    Default,
    ConfigFile,
    EnvVar,
    Cli,
}

impl Config {
    pub fn load(config_path: Option<&Path>) -> Result<Self, ConfigError> {
        let defaults = default_values();

        let mut port = defaults.port;
        let mut upload_token = defaults.upload_token;
        let mut max_file_size = defaults.max_file_size;
        let mut blacklisted_files = defaults.blacklisted_files;
        let mut allowed_extensions = defaults.allowed_extensions;
        let mut root_override: Option<PathBuf> = None;
        let mut config_dir: Option<PathBuf> = None;
        let mut root_source = RootSource::Default;

        let candidates = resolve_config_candidates(config_path)?;

        for candidate in candidates {
            if let Ok(contents) = fs::read_to_string(&candidate) {
                tracing::info!("Loaded configuration from {}", candidate.display());
                let parsed: FileConfig = toml::from_str(&contents)?;

                if let Some(value) = parsed.port {
                    port = value;
                }

                if let Some(value) = parsed.upload_token {
                    upload_token = value;
                }

                if let Some(value) = parsed.max_file_size {
                    max_file_size = value;
                }

                if let Some(values) = parsed.blacklisted_files {
                    let set = values
                        .into_iter()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect::<HashSet<_>>();
                    if !set.is_empty() {
                        blacklisted_files = set;
                    }
                }

                if let Some(values) = parsed.allowed_extensions {
                    let set = values
                        .into_iter()
                        .map(|s| s.trim().to_ascii_lowercase())
                        .filter(|s| !s.is_empty())
                        .collect::<HashSet<_>>();
                    if !set.is_empty() {
                        allowed_extensions = set;
                    }
                }

                if let Some(root) = parsed.root {
                    if !root.trim().is_empty() {
                        root_override = Some(PathBuf::from(&root));
                        root_source = RootSource::ConfigFile;
                    }
                }

                config_dir = candidate.parent().map(|p| p.to_path_buf());
                break;
            }
        }

        if let Ok(value) = env::var("SERVE_PORT") {
            if let Ok(parsed) = value.parse() {
                port = parsed;
            }
        }

        if let Ok(value) = env::var("SERVE_UPLOAD_TOKEN") {
            if !value.is_empty() {
                upload_token = value;
            }
        }

        if let Ok(value) = env::var("SERVE_MAX_FILE_SIZE") {
            if let Ok(parsed) = value.parse() {
                max_file_size = parsed;
            }
        }

        if let Ok(value) = env::var("SERVE_BLACKLIST") {
            let set = value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect::<HashSet<_>>();
            if !set.is_empty() {
                blacklisted_files = set;
            }
        }

        if let Ok(value) = env::var("SERVE_ALLOWED_EXT") {
            let set = value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_lowercase())
                .collect::<HashSet<_>>();
            if !set.is_empty() {
                allowed_extensions = set;
            }
        }

        if let Ok(value) = env::var("SERVE_ROOT") {
            if !value.trim().is_empty() {
                root_override = Some(PathBuf::from(value));
                root_source = RootSource::EnvVar;
            }
        }

        Ok(Self {
            port,
            upload_token,
            max_file_size,
            blacklisted_files,
            allowed_extensions,
            root_override,
            config_dir,
            root_source,
        })
    }

    pub fn storage_dir(&self) -> PathBuf {
        self.config_dir.clone().unwrap_or_else(default_config_dir)
    }
}

struct DefaultValues {
    port: u16,
    upload_token: String,
    max_file_size: u64,
    blacklisted_files: HashSet<String>,
    allowed_extensions: HashSet<String>,
}

fn default_values() -> DefaultValues {
    DefaultValues {
        port: 3435,
        upload_token: "abogoboga".to_string(),
        max_file_size: 4000 * 1024 * 1024,
        blacklisted_files: ["utils", "server.py"]
            .into_iter()
            .map(|s| s.to_string())
            .collect(),
        allowed_extensions: default_allowed_extensions(),
    }
}

fn default_allowed_extensions() -> HashSet<String> {
    [
        "mp3", "wav", "aac", "ogg", "flac", "m4a", "mp4", "avi", "mov", "wmv", "mkv", "flv",
        "webm", "jpg", "jpeg", "png", "gif", "bmp", "tiff", "svg", "zip", "tar", "gz", "bz2", "7z",
        "rar", "exe", "bin", "dll", "deb", "rpm", "iso", "pdf", "doc", "docx", "xls", "xlsx",
        "ppt", "pptx", "txt", "csv", "odt", "rtf", "xml",
    ]
    .into_iter()
    .map(|s| s.to_string())
    .collect()
}

fn resolve_config_candidates(config_path: Option<&Path>) -> Result<Vec<PathBuf>, ConfigError> {
    let mut candidates = Vec::new();

    if let Some(path) = config_path {
        candidates.push(path.to_path_buf());
        return Ok(candidates);
    }

    candidates.push(PathBuf::from("config.toml"));

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("config.toml"));
        }
    }

    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            candidates.push(PathBuf::from(xdg).join("serve").join("config.toml"));
        }
    }

    if let Ok(home) = env::var("HOME") {
        if !home.trim().is_empty() {
            candidates.push(
                PathBuf::from(&home)
                    .join(".config")
                    .join("serve")
                    .join("config.toml"),
            );
            candidates.push(PathBuf::from(home).join(".serve").join("config.toml"));
        }
    }

    Ok(candidates)
}

pub fn default_config_dir() -> PathBuf {
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        if !xdg.trim().is_empty() {
            return PathBuf::from(xdg).join("serve");
        }
    }

    if let Ok(home) = env::var("HOME") {
        if !home.trim().is_empty() {
            return PathBuf::from(&home).join(".config").join("serve");
        }
    }

    if let Ok(home) = env::var("USERPROFILE") {
        if !home.trim().is_empty() {
            return PathBuf::from(home).join(".config").join("serve");
        }
    }

    PathBuf::from(".serve")
}

#[derive(Debug, Deserialize)]
struct FileConfig {
    port: Option<u16>,
    upload_token: Option<String>,
    max_file_size: Option<u64>,
    blacklisted_files: Option<Vec<String>>,
    allowed_extensions: Option<Vec<String>>,
    root: Option<String>,
}

#[derive(Debug)]
pub enum ConfigError {
    Io(std::io::Error),
    ParseToml(toml::de::Error),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Io(err) => write!(f, "Failed to read config file: {err}"),
            ConfigError::ParseToml(err) => write!(f, "Failed to parse config file: {err}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl From<std::io::Error> for ConfigError {
    fn from(err: std::io::Error) -> Self {
        ConfigError::Io(err)
    }
}

impl From<toml::de::Error> for ConfigError {
    fn from(err: toml::de::Error) -> Self {
        ConfigError::ParseToml(err)
    }
}
