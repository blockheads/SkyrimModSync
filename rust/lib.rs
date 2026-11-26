//! Share a Skyrim mod setup with friends. The host's mod list is published as a pack (files by hash,
//! load order, shared INI settings); friends sync it, load it into their game, play, and unload it.
pub mod archive;
pub mod config;
pub mod deploy;
pub mod files;
pub mod hash;
pub mod host;
pub mod ini;
pub mod launch;
pub mod list;
pub mod net;
pub mod pack;
pub mod peer;
pub mod reports;
pub mod rules;
pub mod steam;
pub mod store;
pub mod sync;
#[cfg(test)]
mod tests;
pub mod update;

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
/// --json: progress as JSON lines.
pub static JSON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

type Sink = Box<dyn Fn(&str) + Send + Sync>;
static SINK: std::sync::OnceLock<Sink> = std::sync::OnceLock::new();
/// Progress lines go to `sink` instead of stderr (the GUI).
pub fn set_event_sink(sink: impl Fn(&str) + Send + Sync + 'static) {
    let _ = SINK.set(Box::new(sink));
}

/// A progress line on stderr (the result goes to stdout).
pub fn event(state: &str, detail: serde_json::Value) {
    if JSON.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("{}", serde_json::json!({"state": state, "detail": detail}));
        return;
    }
    let fields = match &detail {
        serde_json::Value::Object(m) => m
            .iter()
            .map(|(k, v)| match v {
                serde_json::Value::String(s) => format!("{k}: {s}"),
                _ => format!("{k}: {v}"),
            })
            .collect::<Vec<_>>()
            .join(", "),
        other => other.to_string(),
    };
    line(&format!("{}: {fields}", state.replace('_', " ")));
}

fn line(text: &str) {
    match SINK.get() {
        Some(sink) => sink(text),
        None => eprintln!("{text}"),
    }
}

/// A progress line for people (`message`); --json gets `state` and `detail` instead.
pub fn say(state: &str, message: &str, detail: serde_json::Value) {
    if JSON.load(Ordering::Relaxed) {
        eprintln!("{}", serde_json::json!({"state": state, "detail": detail}));
    } else {
        line(message);
    }
}

/// A size for people: 1.2 GB, 340 MB.
pub fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} GB", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.0} MB", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} B"),
    }
}

type ProgressSink = Box<dyn Fn(&str, f32) + Send + Sync>;
static PROGRESS: std::sync::OnceLock<ProgressSink> = std::sync::OnceLock::new();
/// Progress (the same text the terminal prints, and how far along) goes to `sink` (the GUI's bar).
pub fn set_progress_sink(sink: impl Fn(&str, f32) + Send + Sync + 'static) {
    let _ = PROGRESS.set(Box::new(sink));
}

/// Bytes arriving for one stage of a sync (several downloads at once add to it).
pub struct Meter {
    label: String,
    total: u64,
    done: std::sync::atomic::AtomicU64,
    start: std::time::Instant,
    last: std::sync::Mutex<std::time::Instant>,
}
impl Meter {
    pub fn new(label: &str, total: u64) -> Self {
        let now = std::time::Instant::now();
        let m = Self {
            label: label.into(),
            total,
            done: 0.into(),
            start: now,
            last: std::sync::Mutex::new(now),
        };
        if let Some(sink) = PROGRESS.get() {
            sink(&m.label, 0.0);
        }
        m
    }
    pub fn add(&self, n: u64) {
        let done = self.done.fetch_add(n, Ordering::Relaxed) + n;
        // The bar redraws often; terminal lines every few seconds.
        let every = if PROGRESS.get().is_some() { 200 } else { 3000 };
        let mut last = self.last.lock().unwrap();
        if last.elapsed().as_millis() >= every {
            *last = std::time::Instant::now();
            drop(last);
            self.report(done);
        }
    }
    /// The stage's last line, the same in the terminal and the window's text.
    pub fn finish(&self) {
        if let Some(sink) = PROGRESS.get() {
            sink(&self.label, 1.0);
        }
        let secs = self.start.elapsed().as_secs_f64().max(0.001);
        say(
            "progress",
            &format!(
                "{}: {} done in {:.0}s ({}/s)",
                self.label,
                size(self.total),
                secs,
                size((self.total as f64 / secs) as u64)
            ),
            serde_json::json!({"stage": self.label, "done": self.total}),
        );
    }
    fn report(&self, done: u64) {
        let done = done.min(self.total);
        let part = if self.total == 0 {
            1.0
        } else {
            done as f32 / self.total as f32
        };
        let mut text = format!(
            "{}: {} of {} ({:.0}%)",
            self.label,
            size(done),
            size(self.total),
            part * 100.0
        );
        let secs = self.start.elapsed().as_secs_f64();
        if done > 0 && done < self.total && secs >= 1.0 {
            let rate = done as f64 / secs;
            let left = ((self.total - done) as f64 / rate) as u64;
            let left = match left {
                s if s >= 3600 => format!("{}h {}m", s / 3600, s % 3600 / 60),
                s if s >= 60 => format!("{}m {}s", s / 60, s % 60),
                s => format!("{s}s"),
            };
            text.push_str(&format!(", {}/s, about {left} left", size(rate as u64)));
        }
        if let Some(sink) = PROGRESS.get() {
            return sink(&text, part);
        }
        say(
            "progress",
            &text,
            serde_json::json!({"stage": self.label, "done": done, "total": self.total}),
        );
    }
}
