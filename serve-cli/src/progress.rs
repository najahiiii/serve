use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub const PARTIAL_STATE_UPDATE_THRESHOLD: u64 = 8 * 1024 * 1024; // 8 MiB

pub fn create_progress_bar(total: Option<u64>, label: &str) -> ProgressBar {
    create_progress_bar_with_message_in(None, total, label, None)
}

pub fn create_progress_bar_with_message_in(
    multi: Option<&MultiProgress>,
    total: Option<u64>,
    label: &str,
    message: Option<String>,
) -> ProgressBar {
    let formatted = format_label(label);
    if let Some(len) = total {
        let pb = match multi {
            Some(multi) => multi.add(ProgressBar::new(len)),
            None => ProgressBar::new(len),
        };
        pb.set_prefix(formatted);
        pb.set_message(message.unwrap_or_default());
        pb.set_style(
            ProgressStyle::with_template(
                "{prefix} {bar:40.cyan/blue} {bytes}/{total_bytes} ({percent:>3}%) [{elapsed_precise}] ({eta}) {bytes_per_sec}{msg}",
            )
            .unwrap()
            .progress_chars("##-"),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        pb
    } else {
        let pb = match multi {
            Some(multi) => multi.add(ProgressBar::new_spinner()),
            None => ProgressBar::new_spinner(),
        };
        pb.set_prefix(formatted);
        pb.set_message(message.unwrap_or_default());
        pb.set_style(
            ProgressStyle::with_template(
                "{prefix} {spinner} {bytes} downloaded [{elapsed_precise}] ({bytes_per_sec}){msg}",
            )
            .unwrap()
            .tick_strings(&["-", "\\", "|", "/"]),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        pb
    }
}

fn format_label(label: &str) -> String {
    const MAX: usize = 25;
    if label.len() <= MAX {
        return label.to_string();
    }
    let mut truncated = label.chars().rev().take(MAX - 3).collect::<String>();
    truncated = truncated.chars().rev().collect();
    format!("...{}", truncated)
}

pub fn connection_status_message(active: usize, total: usize) -> Option<String> {
    if total > 1 {
        Some(format!(" [{}/{} connections]", active, total))
    } else {
        None
    }
}

pub fn update_connection_message(progress: &ProgressBar, active: usize, total: usize) {
    if let Some(msg) = connection_status_message(active, total) {
        progress.set_message(msg);
    } else {
        progress.set_message("");
    }
}

pub fn finish_progress(progress: &ProgressBar, message: &str) {
    progress.finish_and_clear();
    if !message.is_empty() {
        println!("{message}");
    }
}

pub struct ActiveConnectionGuard {
    counter: Arc<AtomicUsize>,
    progress: ProgressBar,
    total: usize,
}

impl ActiveConnectionGuard {
    pub fn new(counter: Arc<AtomicUsize>, progress: &ProgressBar, total: usize) -> Self {
        let current = counter.fetch_add(1, Ordering::SeqCst).saturating_add(1);
        update_connection_message(progress, current, total);
        Self {
            counter,
            progress: progress.clone(),
            total,
        }
    }
}

impl Drop for ActiveConnectionGuard {
    fn drop(&mut self) {
        let remaining = self
            .counter
            .fetch_sub(1, Ordering::SeqCst)
            .saturating_sub(1);
        update_connection_message(&self.progress, remaining, self.total);
    }
}
