use anyhow::{Error, Result};
use reqwest::{Error as ReqwestError, StatusCode};
use std::io::{self, ErrorKind};
use std::thread;
use std::time::Duration;

pub fn retry<T, F>(operation: &str, max_attempts: usize, mut func: F) -> Result<T>
where
    F: FnMut() -> Result<T>,
{
    let attempts = max_attempts.max(1);
    for attempt in 1..=attempts {
        match func() {
            Ok(value) => return Ok(value),
            Err(err) => {
                if attempt == attempts || !is_retryable_error(&err) {
                    return Err(err);
                }
                let delay = retry_delay(attempt);
                eprintln!(
                    "{} failed (attempt {}/{}): {}. Retrying in {}s...",
                    operation,
                    attempt,
                    attempts,
                    err,
                    delay.as_secs()
                );
                thread::sleep(delay);
            }
        }
    }

    unreachable!("retry loop must return success or error")
}

fn retry_delay(attempt: usize) -> Duration {
    let capped = attempt.saturating_sub(1).min(3) as u32;
    Duration::from_secs(1 << capped)
}

fn is_retryable_error(err: &Error) -> bool {
    use ErrorKind::*;

    for cause in err.chain() {
        if let Some(req_err) = cause.downcast_ref::<ReqwestError>() {
            if req_err.is_timeout() || req_err.is_connect() || req_err.is_body() {
                return true;
            }
            if let Some(status) = req_err.status() {
                if status.is_server_error()
                    || status == StatusCode::TOO_MANY_REQUESTS
                    || status == StatusCode::REQUEST_TIMEOUT
                {
                    return true;
                } else {
                    return false;
                }
            }
        }
        if let Some(io_err) = cause.downcast_ref::<io::Error>() {
            match io_err.kind() {
                TimedOut | ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof
                | WouldBlock | Interrupted => {
                    return true;
                }
                _ => {}
            }
        }
    }

    false
}
