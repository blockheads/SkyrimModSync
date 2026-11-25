//! Share a Skyrim mod setup with friends. The host's MO2 profile is published as a pack (files by hash,
//! load order, shared INI settings); friends sync it, load it into their game, play, and unload it.
pub mod archive;
pub mod config;
pub mod deploy;
pub mod files;
pub mod hash;
pub mod host;
pub mod ini;
pub mod launch;
pub mod mo2;
pub mod net;
pub mod pack;
pub mod peer;
pub mod rules;
pub mod steam;
pub mod store;
pub mod sync;
#[cfg(test)]
mod tests;

use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

pub type Result<T> = std::result::Result<T, Error>;

/// An error as data: a stable code and one sentence for people.
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
        Self::new("invalid_archive", e.to_string())
    }
}
impl From<reqwest::Error> for Error {
    fn from(e: reqwest::Error) -> Self {
        Self::new(
            "network_error",
            format!("A download failed ({}).", e.without_url()),
        )
    }
}

pub static CANCELLED: AtomicBool = AtomicBool::new(false);
pub fn check_cancel() -> Result<()> {
    if CANCELLED.load(Ordering::Relaxed) {
        return Err(Error::new(
            "cancelled",
            "Cancelled; finished downloads are kept.",
        ));
    }
    Ok(())
}
/// A progress line on stderr (the result goes to stdout).
pub fn event(state: &str, detail: serde_json::Value) {
    eprintln!("{}", serde_json::json!({"state": state, "detail": detail}));
}
