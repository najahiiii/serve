use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, Once, OnceLock};

static SIGNAL_HANDLER: Once = Once::new();
static TEMP_FILE_REGISTRY: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();

pub struct TempCleanupGuard {
    active: bool,
}

impl TempCleanupGuard {
    pub fn new() -> Self {
        Self { active: true }
    }

    pub fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for TempCleanupGuard {
    fn drop(&mut self) {
        if self.active {
            let kept = abandon_temp_files();
            if !kept.is_empty() {
                eprintln!("download interrupted; partial file(s) preserved:");
                for path in kept {
                    eprintln!("  {}", path.display());
                }
            }
        }
    }
}

pub fn install_signal_handler() {
    SIGNAL_HANDLER.call_once(|| {
        if let Err(err) = ctrlc::set_handler(|| {
            let kept = abandon_temp_files();
            if kept.is_empty() {
                eprintln!("Operation cancelled.");
            } else {
                eprintln!("Operation cancelled; partial file(s) preserved:");
                for path in kept {
                    eprintln!("  {}", path.display());
                }
            }
            std::process::exit(130);
        }) {
            eprintln!("failed to install Ctrl+C handler: {err}");
        }
    });
}

fn temp_registry() -> &'static Mutex<HashSet<PathBuf>> {
    TEMP_FILE_REGISTRY.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn track_temp_file(path: &Path) {
    if let Ok(mut registry) = temp_registry().lock() {
        registry.insert(path.to_path_buf());
    }
}

pub fn untrack_temp_file(path: &Path) {
    if let Ok(mut registry) = temp_registry().lock() {
        registry.remove(path);
    }
}

pub fn abandon_temp_files() -> Vec<PathBuf> {
    drain_tracked_temp_files()
}

fn drain_tracked_temp_files() -> Vec<PathBuf> {
    let mut to_remove = Vec::new();
    if let Ok(mut registry) = temp_registry().lock() {
        to_remove.extend(registry.drain());
    }
    to_remove
}
