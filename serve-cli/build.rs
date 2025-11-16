use std::env;
use std::process::Command;
use time::{OffsetDateTime, UtcOffset, format_description};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");

    set_env(
        "SERVE_CLI_RUSTC_VERSION",
        rustc_version().unwrap_or_else(|| "unknown".into()),
    );
    set_env(
        "SERVE_CLI_GIT_COMMIT",
        git_commit().unwrap_or_else(|| "unknown".into()),
    );
    set_env("SERVE_CLI_BUILD_TIME", build_timestamp());
    set_env(
        "SERVE_CLI_TARGET_OS",
        env::var("CARGO_CFG_TARGET_OS").unwrap_or_else(|_| "unknown".into()),
    );
    set_env(
        "SERVE_CLI_TARGET_ARCH",
        env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "unknown".into()),
    );
}

fn rustc_version() -> Option<String> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let output = Command::new(rustc).arg("--version").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut parts = text.split_whitespace();
    let tool = parts.next().unwrap_or("rustc");
    let version = parts.next().unwrap_or("");
    let short = format!("{} {}", tool, version).trim().to_string();
    if short.is_empty() { None } else { Some(short) }
}

fn git_commit() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if hash.is_empty() { None } else { Some(hash) }
}

fn build_timestamp() -> String {
    let offset = UtcOffset::from_hms(7, 0, 0).unwrap();
    let wib_time = OffsetDateTime::now_utc().to_offset(offset);
    let format =
        format_description::parse("[year]-[month]-[day] [hour]:[minute]:[second]").unwrap();
    wib_time
        .format(&format)
        .unwrap_or_else(|_| "unknown".into())
}

fn set_env(key: &str, value: String) {
    println!("cargo:rustc-env={}={}", key, value);
}
