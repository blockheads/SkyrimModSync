pub mod acquisition;
pub mod authoring;
pub mod config;
pub mod engine;
pub mod files;
pub mod json;
pub mod package;
pub mod peer;
pub mod play;
pub mod recipe;
pub mod repair;
pub mod revision;
pub mod snapshot;
pub mod target;

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Serialize)]
pub struct Error {
    pub code: String,
    pub message: String,
}
impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::new(
            if e.raw_os_error() == Some(28) {
                "disk_full"
            } else {
                "io_error"
            },
            e.to_string(),
        )
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new("invalid_json", e.to_string())
    }
}
impl From<zip::result::ZipError> for Error {
    fn from(e: zip::result::ZipError) -> Self {
        Self::new("invalid_package", e.to_string())
    }
}
impl From<reqwest::Error> for Error {
    fn from(_: reqwest::Error) -> Self {
        Self::new("network_error", "HTTPS request failed; check connectivity and retry. Credentials and signed URLs are omitted from errors.")
    }
}
pub static CANCELLED: AtomicBool = AtomicBool::new(false);
pub fn check_cancel() -> Result<()> {
    if CANCELLED.load(Ordering::Relaxed) {
        Err(Error::new(
            "cancelled",
            "Cancelled; cached downloads and the previous installation are retained.",
        ))
    } else {
        Ok(())
    }
}
pub fn event(state: &str, detail: serde_json::Value) {
    eprintln!("{}", serde_json::json!({"state":state,"detail":detail}));
}
