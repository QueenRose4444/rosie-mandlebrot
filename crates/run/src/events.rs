//! What a stage reports while it runs.
//!
//! Every stage takes an [`Events`] sink. [`JsonLines`] writes one JSON object
//! per line, which is how a parent process (the web UI's job runner) follows a
//! child: the lines start with `{"ev":`, so they are told apart from log lines
//! on the same stream.

use serde::Serialize;
use std::io::Write;
use std::sync::Mutex;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    /// A stage began. `frames` is the number of video frames when known.
    Started { stage: &'static str, width: u32, height: u32, frames: Option<u64> },
    /// One strip chunk is on disk.
    Chunk {
        index: u32,
        rows: u64,
        /// Video frames the strip can serve so far, and the span of the last.
        frames: u64,
        span: f64,
        max_iter: u32,
        shader_ms: f64,
        render_ms: f64,
        bytes: u64,
        elapsed_secs: f64,
    },
    /// One video frame was assembled (`k` is 0-based) or rendered (frame by
    /// frame, 1-based like the PNG names).
    Frame { k: u64, frames: Option<u64>, span: f64 },
    /// A PNG was written: an assembled frame asked for, or a preview.
    Image { path: String, k: u64 },
    /// The video file is complete.
    Video { path: String, frames: u64 },
    /// The finder confirmed a place: a minibrot nucleus to zoom into.
    Place(Box<PlaceEvent>),
    /// A stage ended. `status` is "stopped" or "failed", as in progress.json.
    Finished { stage: &'static str, status: &'static str, reason: String, elapsed_secs: f64 },
}

/// One place from the finder (boxed in [`Event::Place`]: it is much larger
/// than the other events).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlaceEvent {
    /// 1-based, in the order found.
    pub n: u32,
    pub re: String,
    pub im: String,
    pub period: u32,
    /// Predicted end of structure: the span at which the minibrot fills the
    /// frame.
    pub end: f64,
    /// How interesting, 0–100.
    pub interest: f64,
    /// The numbers the interest comes from.
    pub events: u32,
    pub events_per_decade: f64,
    pub close_share: f64,
    pub longest_gap: f64,
    /// Mean and least detail of the pictures down the zoom (0 = flat).
    pub detail: f64,
    pub detail_min: f64,
    /// Search level (0 = directly below the starting place).
    pub level: u32,
    /// Pictures down the zoom, shallowest first; the last is near the end.
    pub pictures: Vec<String>,
}

/// Where a stage's events go. Must not block for long: stages call it inline.
pub trait Events: Sync {
    fn emit(&self, e: Event);
}

/// Discards everything.
pub struct NoEvents;

impl Events for NoEvents {
    fn emit(&self, _: Event) {}
}

/// One JSON object per line on the given writer (stdout for a child process).
pub struct JsonLines<W: Write + Send>(pub Mutex<W>);

impl<W: Write + Send> JsonLines<W> {
    pub fn new(w: W) -> Self {
        JsonLines(Mutex::new(w))
    }
}

impl<W: Write + Send> Events for JsonLines<W> {
    fn emit(&self, e: Event) {
        if let (Ok(mut w), Ok(line)) = (self.0.lock(), serde_json::to_string(&e)) {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }
}

impl<F: Fn(Event) + Sync> Events for F {
    fn emit(&self, e: Event) {
        self(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_one_tagged_json_object_per_line() {
        let sink = JsonLines::new(Vec::new());
        sink.emit(Event::Frame { k: 3, frames: Some(10), span: 0.5 });
        sink.emit(Event::Finished { stage: "strip", status: "stopped", reason: "zoom end reached".into(), elapsed_secs: 1.0 });
        let out = String::from_utf8(sink.0.into_inner().unwrap()).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.starts_with("{\"ev\":")), "{out}");
        assert_eq!(lines[0], r#"{"ev":"frame","k":3,"frames":10,"span":0.5}"#);
    }
}
