//! Events module: structured event log for simulator operations.
//!
//! Every op lifecycle emits one [`Event`]. Sinks consume events without
//! returning errors so the hot FUSE path never blocks on I/O failures.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use serde::Serialize;

use crate::op::FsOp;

/// Result classification for an event.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Ok,
    Error,
}

/// A single op lifecycle event.
#[derive(Clone, Debug, Serialize)]
pub struct Event {
    /// Wall-clock timestamp of completion, nanoseconds since UNIX epoch.
    pub ts_unix_nanos: u128,
    /// Monotonic sequence number assigned by the engine.
    pub seq: u64,
    /// The operation kind.
    pub op: FsOp,
    /// Target inode.
    pub ino: u64,
    /// Byte offset (0 for metadata ops).
    pub offset: u64,
    /// Request length in bytes (0 for metadata ops).
    pub len: usize,
    /// Whether the op succeeded or errored.
    pub outcome: Outcome,
    /// Errno, present only for Error outcomes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errno: Option<i32>,
    /// Time spent waiting for a concurrency slot.
    pub queue_wait_us: u64,
    /// Latency injected by the sampler.
    pub injected_latency_us: u64,
    /// Delay attributable to bandwidth throttling.
    pub bandwidth_delay_us: u64,
    /// Total wall-clock duration of the op from acquire to reply.
    pub total_duration_us: u64,
}

/// Consumer of [`Event`]s.
pub trait EventSink: Send + Sync + 'static {
    /// Record an event. Implementations must not panic and must not return
    /// errors — the engine is on the hot FUSE path.
    fn emit(&self, event: &Event);
}

/// No-op sink that discards every event.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullEventSink;

impl EventSink for NullEventSink {
    fn emit(&self, _: &Event) {}
}

/// JSON-lines event sink writing to a local file.
///
/// The file is truncated on construction and buffered; call [`Self::flush`] on
/// shutdown to guarantee all events are persisted.
pub struct JsonlEventSink {
    inner: Mutex<BufWriter<File>>,
}

impl JsonlEventSink {
    /// Create (or truncate) the file at `path` and wrap it in a buffered sink.
    pub fn create<P: AsRef<Path>>(path: P) -> std::io::Result<Self> {
        let file = File::create(path)?;
        Ok(Self {
            inner: Mutex::new(BufWriter::new(file)),
        })
    }

    /// Flush any buffered writes to disk.
    pub fn flush(&self) -> std::io::Result<()> {
        self.inner.lock().flush()
    }
}

impl EventSink for JsonlEventSink {
    fn emit(&self, event: &Event) {
        // Serialization errors are suppressed; we prefer dropping the event to
        // returning from a FUSE handler with a spurious IO failure.
        if let Ok(json) = serde_json::to_string(event) {
            let mut w = self.inner.lock();
            let _ = writeln!(*w, "{json}");
        }
    }
}

impl std::fmt::Debug for JsonlEventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlEventSink").finish_non_exhaustive()
    }
}

/// Current wall-clock time as nanoseconds since UNIX epoch.
pub fn now_unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_path(tag: &str) -> PathBuf {
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "fusecraft-jsonl-{}-{}-{}",
            std::process::id(),
            tag,
            id
        ))
    }

    fn sample_event(seq: u64, outcome: Outcome, errno: Option<i32>) -> Event {
        Event {
            ts_unix_nanos: 1_700_000_000_000_000_000,
            seq,
            op: FsOp::Read,
            ino: 100,
            offset: 0,
            len: 4096,
            outcome,
            errno,
            queue_wait_us: 1,
            injected_latency_us: 42,
            bandwidth_delay_us: 3,
            total_duration_us: 50,
        }
    }

    #[test]
    fn null_sink_accepts_events() {
        let sink = NullEventSink;
        sink.emit(&sample_event(1, Outcome::Ok, None));
    }

    #[test]
    fn jsonl_sink_writes_one_line_per_event() {
        let path = temp_path("write-per-event");
        let _ = fs::remove_file(&path);

        let sink = JsonlEventSink::create(&path).unwrap();
        sink.emit(&sample_event(1, Outcome::Ok, None));
        sink.emit(&sample_event(2, Outcome::Error, Some(libc::EIO)));
        sink.flush().unwrap();
        drop(sink);

        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 2);

        let v1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v1["seq"], 1);
        assert_eq!(v1["op"], "read");
        assert_eq!(v1["outcome"], "ok");
        assert!(v1.get("errno").is_none()); // skipped when None

        let v2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(v2["seq"], 2);
        assert_eq!(v2["outcome"], "error");
        assert_eq!(v2["errno"], libc::EIO);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn jsonl_sink_truncates_on_create() {
        let path = temp_path("truncate");
        let _ = fs::remove_file(&path);

        // First run writes one event.
        {
            let sink = JsonlEventSink::create(&path).unwrap();
            sink.emit(&sample_event(1, Outcome::Ok, None));
            sink.flush().unwrap();
        }

        // Second create should truncate — only the new event remains.
        {
            let sink = JsonlEventSink::create(&path).unwrap();
            sink.emit(&sample_event(99, Outcome::Ok, None));
            sink.flush().unwrap();
        }

        let contents = fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 1);
        let v: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(v["seq"], 99);

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn sink_usable_as_trait_object() {
        let path = temp_path("trait-object");
        let _ = fs::remove_file(&path);

        let sink: Box<dyn EventSink> = Box::new(JsonlEventSink::create(&path).unwrap());
        sink.emit(&sample_event(7, Outcome::Ok, None));
        // Dropping flushes via BufWriter's Drop.
        drop(sink);

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents.lines().count(), 1);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn now_unix_nanos_is_positive() {
        assert!(now_unix_nanos() > 0);
    }
}
