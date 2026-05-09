//! Token-bucket bandwidth limiter.
//!
//! Returns a [`Duration`] indicating how long the caller must wait before
//! proceeding — the limiter itself does **not** sleep. This allows the engine
//! layer to coordinate sleep with other concerns (e.g., latency injection).

use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::config::BandwidthProfile;

/// Statistics snapshot for a [`BandwidthLimiter`].
#[derive(Clone, Debug, PartialEq)]
pub struct BandwidthStats {
    /// Current token balance in bytes.
    pub tokens_available: f64,
    /// Configured refill rate in bytes per second.
    pub bytes_per_sec: f64,
    /// Configured burst capacity in bytes.
    pub burst_bytes: u64,
    /// Total bytes reserved since creation.
    pub total_bytes_reserved: u64,
    /// Total number of reservations that required a non-zero wait.
    pub total_throttled: u64,
}

/// Internal mutable state.
struct BucketState {
    /// Current token balance (fractional bytes allowed; may go negative).
    tokens: f64,
    /// Last time tokens were refilled.
    last: Instant,
    /// Total bytes reserved.
    total_bytes_reserved: u64,
    /// Total throttled operations.
    total_throttled: u64,
}

/// A token-bucket bandwidth limiter.
///
/// Limits throughput to a configured bytes-per-second rate with burst
/// allowance. The [`reserve`](BandwidthLimiter::reserve) method returns a
/// [`Duration`] indicating how long the caller should wait, but does **not**
/// sleep internally.
///
/// If `rate <= 0.0`, the limiter degrades to no-throttle (always returns
/// `Duration::ZERO`).
///
/// # Thread Safety
///
/// This type is `Send + Sync` and designed for use from multiple FUSE handler
/// threads simultaneously.
pub struct BandwidthLimiter {
    rate: f64,
    capacity: f64,
    state: Mutex<BucketState>,
}

impl BandwidthLimiter {
    /// Create a new bandwidth limiter from a [`BandwidthProfile`].
    ///
    /// Does not panic on invalid profiles — degrades gracefully:
    /// - `bytes_per_sec <= 0.0`: no throttling (always returns `Duration::ZERO`).
    /// - `burst_bytes == 0`: bucket always throttles for any positive request.
    pub fn new(profile: &BandwidthProfile) -> Self {
        Self {
            rate: profile.bytes_per_sec,
            capacity: profile.burst_bytes as f64,
            state: Mutex::new(BucketState {
                tokens: profile.burst_bytes as f64,
                last: Instant::now(),
                total_bytes_reserved: 0,
                total_throttled: 0,
            }),
        }
    }

    /// Reserve `bytes` worth of bandwidth.
    ///
    /// Returns the [`Duration`] the caller must wait before sending those bytes.
    /// A return value of [`Duration::ZERO`] means the caller may proceed
    /// immediately.
    ///
    /// This method **does not sleep** — the caller is responsible for sleeping
    /// or otherwise delaying.
    pub fn reserve(&self, bytes: u64) -> Duration {
        // Zero-byte reservations are a true no-op: never touch state, never
        // delay (even if the bucket is currently in debt from earlier callers).
        if bytes == 0 {
            return Duration::ZERO;
        }

        let mut state = self.state.lock();
        // Account bytes up front so unlimited-rate profiles still report
        // throughput in `stats()`. `saturating_add` avoids wraparound on
        // pathological workloads that reserve more than u64::MAX cumulatively.
        state.total_bytes_reserved = state.total_bytes_reserved.saturating_add(bytes);

        // Guard: if rate is non-positive, no throttling possible.
        if self.rate <= 0.0 {
            return Duration::ZERO;
        }

        self.refill(&mut state);

        let needed = bytes as f64;

        // Subtract tokens; may go negative.
        state.tokens -= needed;

        if state.tokens >= 0.0 {
            Duration::ZERO
        } else {
            state.total_throttled += 1;
            let deficit = -state.tokens;
            let wait_secs = deficit / self.rate;
            // `try_from_secs_f64` avoids a panic for a pathologically small
            // positive rate combined with a very large deficit (which would
            // overflow `Duration`). Saturate to `Duration::MAX` instead.
            Duration::try_from_secs_f64(wait_secs).unwrap_or(Duration::MAX)
        }
    }

    /// Return a snapshot of the limiter's current statistics.
    pub fn stats(&self) -> BandwidthStats {
        let mut state = self.state.lock();
        self.refill(&mut state);
        BandwidthStats {
            tokens_available: state.tokens.max(0.0),
            bytes_per_sec: self.rate,
            burst_bytes: self.capacity as u64,
            total_bytes_reserved: state.total_bytes_reserved,
            total_throttled: state.total_throttled,
        }
    }

    /// Refill tokens based on elapsed time since last refill.
    fn refill(&self, state: &mut BucketState) {
        let now = Instant::now();
        let elapsed = now.duration_since(state.last);
        if elapsed.is_zero() {
            return;
        }
        if self.rate > 0.0 {
            let added = elapsed.as_secs_f64() * self.rate;
            state.tokens = (state.tokens + added).min(self.capacity);
        }
        state.last = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn test_profile(bytes_per_sec: f64, burst_bytes: u64) -> BandwidthProfile {
        BandwidthProfile {
            bytes_per_sec,
            burst_bytes,
        }
    }

    #[test]
    fn bandwidth_within_burst_no_delay() {
        let profile = test_profile(1_000_000.0, 4096);
        let limiter = BandwidthLimiter::new(&profile);
        let wait = limiter.reserve(4096);
        assert_eq!(wait, Duration::ZERO);
    }

    #[test]
    fn bandwidth_over_burst_delays() {
        let profile = test_profile(1_000.0, 1_000);
        let limiter = BandwidthLimiter::new(&profile);

        // Exhaust the burst.
        let wait = limiter.reserve(1_000);
        assert_eq!(wait, Duration::ZERO);

        // Next reserve should require waiting.
        let wait = limiter.reserve(500);
        assert!(wait > Duration::ZERO);
        // At 1000 bytes/sec, 500 bytes needs ~0.5s.
        let expected = Duration::from_millis(500);
        let diff = if wait > expected {
            wait - expected
        } else {
            expected - wait
        };
        assert!(
            diff < Duration::from_millis(10),
            "expected ~500ms, got {wait:?}"
        );
    }

    #[test]
    fn bandwidth_refills_over_time() {
        let profile = test_profile(10_000.0, 10_000);
        let limiter = BandwidthLimiter::new(&profile);

        // Exhaust tokens.
        let wait = limiter.reserve(10_000);
        assert_eq!(wait, Duration::ZERO);

        // Wait for some refill.
        thread::sleep(Duration::from_millis(100));

        // Should have ~1000 tokens refilled (10000/sec * 0.1s).
        let wait = limiter.reserve(500);
        assert_eq!(wait, Duration::ZERO);
    }

    #[test]
    fn burst_cap_limits_token_accumulation() {
        let profile = test_profile(100_000.0, 1_000);
        let limiter = BandwidthLimiter::new(&profile);

        // Wait long enough to potentially over-fill.
        thread::sleep(Duration::from_millis(100));

        let stats = limiter.stats();
        assert!(
            stats.tokens_available <= 1000.0,
            "tokens {} exceeded burst cap 1000",
            stats.tokens_available
        );
    }

    #[test]
    fn stats_tracking() {
        let profile = test_profile(1_000_000.0, 4096);
        let limiter = BandwidthLimiter::new(&profile);

        limiter.reserve(1000);
        limiter.reserve(2000);

        let stats = limiter.stats();
        assert_eq!(stats.total_bytes_reserved, 3000);
        assert_eq!(stats.bytes_per_sec, 1_000_000.0);
        assert_eq!(stats.burst_bytes, 4096);
    }

    #[test]
    fn concurrent_reservations_are_serialized() {
        let profile = test_profile(1_000_000.0, 100_000);
        let limiter = Arc::new(BandwidthLimiter::new(&profile));

        let mut handles = Vec::new();
        for _ in 0..10 {
            let lim = Arc::clone(&limiter);
            handles.push(thread::spawn(move || {
                for _ in 0..100 {
                    let _ = lim.reserve(100);
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let stats = limiter.stats();
        assert_eq!(stats.total_bytes_reserved, 100_000);
    }

    #[test]
    fn zero_reserve_returns_zero_duration() {
        let profile = test_profile(1_000.0, 1_000);
        let limiter = BandwidthLimiter::new(&profile);
        let wait = limiter.reserve(0);
        assert_eq!(wait, Duration::ZERO);
    }

    #[test]
    fn large_reserve_returns_proportional_wait() {
        let profile = test_profile(1_000.0, 100);
        let limiter = BandwidthLimiter::new(&profile);

        // Reserve 100 (burst) — immediate.
        let wait = limiter.reserve(100);
        assert_eq!(wait, Duration::ZERO);

        // Reserve 2000 bytes at 1000 bytes/sec -> ~2s wait.
        let wait = limiter.reserve(2000);
        let expected = Duration::from_secs(2);
        let diff = if wait > expected {
            wait - expected
        } else {
            expected - wait
        };
        assert!(
            diff < Duration::from_millis(50),
            "expected ~2s, got {wait:?}"
        );
    }

    #[test]
    fn bandwidth_zero_rate_returns_zero_duration() {
        let profile = test_profile(0.0, 0);
        let limiter = BandwidthLimiter::new(&profile);
        let wait = limiter.reserve(1000);
        assert_eq!(wait, Duration::ZERO);
    }

    #[test]
    fn bandwidth_negative_rate_returns_zero_duration() {
        let profile = test_profile(-100.0, 1000);
        let limiter = BandwidthLimiter::new(&profile);
        let wait = limiter.reserve(5000);
        assert_eq!(wait, Duration::ZERO);
    }
}
