//! Metrics module: per-op counters and latency histograms.
//!
//! Uses HdrHistogram with microsecond resolution across 1µs..60s at 3 sigfigs.
//! Thread-safe via a single `parking_lot::Mutex` around the per-op map.

use std::collections::HashMap;

use hdrhistogram::Histogram;
use parking_lot::Mutex;

use crate::op::FsOp;

/// Histogram lower bound in microseconds.
const HIST_LOW: u64 = 1;
/// Histogram upper bound in microseconds (60 seconds).
const HIST_HIGH: u64 = 60_000_000;
/// Histogram significant figures.
const HIST_SIGFIG: u8 = 3;

/// Per-operation state: two counters and a latency histogram.
#[derive(Debug)]
pub struct OpHisto {
    /// Number of successful completions recorded.
    pub count_ok: u64,
    /// Number of error completions recorded.
    pub count_err: u64,
    /// Histogram of total-duration microseconds across ok and error outcomes.
    pub histo: Histogram<u64>,
}

/// Summary snapshot for one op.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpSummary {
    pub op: FsOp,
    pub count: u64,
    pub errors: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
}

/// Per-op histogram recorder shared across FUSE handler threads.
pub struct HistogramRecorder {
    per_op: Mutex<HashMap<FsOp, OpHisto>>,
}

impl HistogramRecorder {
    /// Create an empty recorder.
    pub fn new() -> Self {
        Self {
            per_op: Mutex::new(HashMap::new()),
        }
    }

    /// Record a single op outcome.
    ///
    /// `duration_us` is clamped to the histogram's range before recording, so
    /// values outside `[1, 60_000_000]` are saturated silently.
    pub fn record(&self, op: FsOp, duration_us: u64, is_error: bool) {
        let mut map = self.per_op.lock();
        let entry = map.entry(op).or_insert_with(|| OpHisto {
            count_ok: 0,
            count_err: 0,
            histo: Histogram::<u64>::new_with_bounds(HIST_LOW, HIST_HIGH, HIST_SIGFIG)
                .expect("valid histogram bounds"),
        });
        if is_error {
            entry.count_err += 1;
        } else {
            entry.count_ok += 1;
        }
        let clamped = duration_us.clamp(HIST_LOW, HIST_HIGH);
        let _ = entry.histo.record(clamped);
    }

    /// Produce summaries for each recorded op, in [`FsOp::ALL`] order.
    pub fn summary(&self) -> Vec<OpSummary> {
        let map = self.per_op.lock();
        FsOp::ALL
            .iter()
            .filter_map(|&op| {
                let entry = map.get(&op)?;
                let count = entry.count_ok + entry.count_err;
                if count == 0 {
                    return None;
                }
                Some(OpSummary {
                    op,
                    count,
                    errors: entry.count_err,
                    p50_us: entry.histo.value_at_percentile(50.0),
                    p95_us: entry.histo.value_at_percentile(95.0),
                    p99_us: entry.histo.value_at_percentile(99.0),
                    p999_us: entry.histo.value_at_percentile(99.9),
                })
            })
            .collect()
    }

    /// Print a one-line-per-op summary to stderr.
    pub fn print_summary(&self) {
        for s in self.summary() {
            eprintln!(
                "op={} count={} p50_us={} p95_us={} p99_us={} p999_us={} errors={}",
                s.op.as_str(),
                s.count,
                s.p50_us,
                s.p95_us,
                s.p99_us,
                s.p999_us,
                s.errors,
            );
        }
    }
}

impl Default for HistogramRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for HistogramRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistogramRecorder").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_empty_when_no_records() {
        let r = HistogramRecorder::new();
        assert!(r.summary().is_empty());
    }

    #[test]
    fn records_ok_and_error_separately() {
        let r = HistogramRecorder::new();
        r.record(FsOp::Read, 100, false);
        r.record(FsOp::Read, 200, false);
        r.record(FsOp::Read, 300, true);

        let s = &r.summary()[0];
        assert_eq!(s.op, FsOp::Read);
        assert_eq!(s.count, 3);
        assert_eq!(s.errors, 1);
    }

    #[test]
    fn summary_skips_ops_without_records() {
        let r = HistogramRecorder::new();
        r.record(FsOp::Read, 100, false);
        let summary = r.summary();
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0].op, FsOp::Read);
    }

    #[test]
    fn summary_ordered_by_fsop_all() {
        let r = HistogramRecorder::new();
        // Record out of FsOp::ALL order on purpose.
        r.record(FsOp::Write, 500, false);
        r.record(FsOp::Read, 100, false);
        r.record(FsOp::Open, 10, false);

        let ops: Vec<_> = r.summary().into_iter().map(|s| s.op).collect();
        // The first of these in FsOp::ALL is Open, then Read, then Write.
        assert_eq!(ops, vec![FsOp::Open, FsOp::Read, FsOp::Write]);
    }

    #[test]
    fn percentiles_are_reasonable() {
        let r = HistogramRecorder::new();
        for i in 1..=1000 {
            r.record(FsOp::Read, i, false);
        }
        let s = &r.summary()[0];
        assert!(s.p50_us >= 450 && s.p50_us <= 550);
        assert!(s.p95_us >= 900 && s.p95_us <= 1000);
        assert!(s.p99_us >= 950 && s.p99_us <= 1010);
    }

    #[test]
    fn clamps_values_outside_range() {
        let r = HistogramRecorder::new();
        r.record(FsOp::Read, 0, false);
        r.record(FsOp::Read, HIST_HIGH * 10, false);
        let s = &r.summary()[0];
        // Min should be 1 after clamping; max should be close to HIST_HIGH.
        assert!(s.p50_us >= HIST_LOW);
        assert!(s.p999_us <= HIST_HIGH + (HIST_HIGH / 1000));
    }

    #[test]
    fn records_multiple_ops_independently() {
        let r = HistogramRecorder::new();
        r.record(FsOp::Read, 100, false);
        r.record(FsOp::Write, 500, false);
        let summary = r.summary();
        assert_eq!(summary.len(), 2);
        let read = summary.iter().find(|s| s.op == FsOp::Read).unwrap();
        let write = summary.iter().find(|s| s.op == FsOp::Write).unwrap();
        assert!(read.p50_us < write.p50_us);
    }
}
