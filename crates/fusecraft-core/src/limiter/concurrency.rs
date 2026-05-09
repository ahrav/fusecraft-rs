//! Blocking concurrency limiter using a semaphore-like pattern.
//!
//! Designed to be called from FUSE handler threads. Uses real `std::thread`
//! blocking via `parking_lot::{Mutex, Condvar}`.

use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::error::FsError;

/// Statistics snapshot for a [`BlockingLimiter`].
#[derive(Copy, Clone, Debug, Default)]
pub struct LimiterStats {
    /// Number of operations currently holding a slot.
    pub inflight: usize,
    /// Number of operations waiting in the queue for a slot.
    pub queued: usize,
    /// Total number of operations that have been acquired.
    pub total_acquired: u64,
    /// Total number of operations rejected due to a full queue.
    pub total_rejected: u64,
    /// Cumulative queue wait time across all admitted acquisitions.
    pub total_queue_wait: Duration,
}

/// Internal mutable state protected by the mutex.
struct LimiterState {
    inflight: usize,
    queued: usize,
    total_acquired: u64,
    total_rejected: u64,
    total_queue_wait_ns: u64,
}

/// A blocking concurrency limiter.
///
/// Limits the number of concurrent in-flight operations. When the concurrency
/// cap is reached, callers block (up to `queue_cap` waiters). If the queue is
/// also full, [`FsError::Errno(libc::EAGAIN)`] is returned immediately.
///
/// If `cap == 0`, all acquisitions are rejected immediately (defense-in-depth).
///
/// # Thread Safety
///
/// This type is `Send + Sync` and designed for use from multiple FUSE handler
/// threads simultaneously.
pub struct BlockingLimiter {
    state: Mutex<LimiterState>,
    cv: Condvar,
    cap: usize,
    queue_cap: usize,
}

impl std::fmt::Debug for BlockingLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlockingLimiter")
            .field("cap", &self.cap)
            .field("queue_cap", &self.queue_cap)
            .finish_non_exhaustive()
    }
}

impl BlockingLimiter {
    /// Create a new limiter with the given concurrency capacity and queue cap.
    ///
    /// A `cap` of zero means all acquisitions will be rejected immediately.
    pub fn new(cap: usize, queue_cap: usize) -> Self {
        Self {
            state: Mutex::new(LimiterState {
                inflight: 0,
                queued: 0,
                total_acquired: 0,
                total_rejected: 0,
                total_queue_wait_ns: 0,
            }),
            cv: Condvar::new(),
            cap,
            queue_cap,
        }
    }

    /// Acquire a concurrency slot, blocking if necessary.
    ///
    /// Returns a [`LimiterGuard`] that releases the slot on drop.
    /// The guard's `queue_wait` field records how long the caller waited.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::Errno(libc::EAGAIN)`] if:
    /// - `cap == 0` (always rejects)
    /// - The queue is full and no slot is immediately available.
    pub fn acquire(&self) -> Result<LimiterGuard<'_>, FsError> {
        let mut state = self.state.lock();

        // Hard reject when cap is zero.
        if self.cap == 0 {
            state.total_rejected += 1;
            return Err(FsError::Errno(libc::EAGAIN));
        }

        // Fast path: slot available.
        if state.inflight < self.cap {
            state.inflight += 1;
            state.total_acquired += 1;
            return Ok(LimiterGuard {
                limiter: self,
                queue_wait: Duration::ZERO,
            });
        }

        // Check if we can queue.
        if state.queued >= self.queue_cap {
            state.total_rejected += 1;
            return Err(FsError::Errno(libc::EAGAIN));
        }

        // Enter the queue and wait.
        state.queued += 1;
        let start = Instant::now();
        loop {
            self.cv.wait(&mut state);
            if state.inflight < self.cap {
                state.inflight += 1;
                state.queued -= 1;
                state.total_acquired += 1;
                let queue_wait = start.elapsed();
                state.total_queue_wait_ns += queue_wait.as_nanos() as u64;
                return Ok(LimiterGuard {
                    limiter: self,
                    queue_wait,
                });
            }
            // Spurious wakeup — keep waiting.
        }
    }

    /// Try to acquire a concurrency slot with a timeout.
    ///
    /// Returns a [`LimiterGuard`] on success, or [`FsError::Errno(libc::EAGAIN)`]
    /// if the timeout expires or the queue is full.
    pub fn acquire_timeout(&self, timeout: Duration) -> Result<LimiterGuard<'_>, FsError> {
        let mut state = self.state.lock();

        if self.cap == 0 {
            state.total_rejected += 1;
            return Err(FsError::Errno(libc::EAGAIN));
        }

        // Fast path: slot available.
        if state.inflight < self.cap {
            state.inflight += 1;
            state.total_acquired += 1;
            return Ok(LimiterGuard {
                limiter: self,
                queue_wait: Duration::ZERO,
            });
        }

        // Check if we can queue.
        if state.queued >= self.queue_cap {
            state.total_rejected += 1;
            return Err(FsError::Errno(libc::EAGAIN));
        }

        // Enter the queue and wait with timeout.
        state.queued += 1;
        let start = Instant::now();
        let deadline = start + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                state.queued -= 1;
                state.total_rejected += 1;
                return Err(FsError::Errno(libc::EAGAIN));
            }
            let result = self.cv.wait_for(&mut state, remaining);
            if state.inflight < self.cap {
                state.inflight += 1;
                state.queued -= 1;
                state.total_acquired += 1;
                let queue_wait = start.elapsed();
                state.total_queue_wait_ns += queue_wait.as_nanos() as u64;
                return Ok(LimiterGuard {
                    limiter: self,
                    queue_wait,
                });
            }
            if result.timed_out() {
                state.queued -= 1;
                state.total_rejected += 1;
                return Err(FsError::Errno(libc::EAGAIN));
            }
        }
    }

    /// Return a snapshot of the limiter's current statistics.
    pub fn stats(&self) -> LimiterStats {
        let state = self.state.lock();
        LimiterStats {
            inflight: state.inflight,
            queued: state.queued,
            total_acquired: state.total_acquired,
            total_rejected: state.total_rejected,
            total_queue_wait: Duration::from_nanos(state.total_queue_wait_ns),
        }
    }

    /// Release a slot back into the pool (called by `LimiterGuard::drop`).
    fn release(&self) {
        let mut state = self.state.lock();
        state.inflight -= 1;
        self.cv.notify_one();
    }
}

/// RAII guard that holds a concurrency slot.
///
/// Releases the slot back to the [`BlockingLimiter`] on drop.
#[derive(Debug)]
pub struct LimiterGuard<'a> {
    limiter: &'a BlockingLimiter,
    /// How long this acquisition waited in the queue before being admitted.
    pub queue_wait: Duration,
}

impl Drop for LimiterGuard<'_> {
    fn drop(&mut self) {
        self.limiter.release();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn acquire_under_cap_succeeds() {
        let limiter = BlockingLimiter::new(2, 0);
        {
            let g1 = limiter.acquire().unwrap();
            let g2 = limiter.acquire().unwrap();
            assert_eq!(g1.queue_wait, Duration::ZERO);
            assert_eq!(g2.queue_wait, Duration::ZERO);
            let stats = limiter.stats();
            assert_eq!(stats.inflight, 2);
            assert_eq!(stats.queued, 0);
        }
        let stats = limiter.stats();
        assert_eq!(stats.inflight, 0);
    }

    #[test]
    fn acquire_over_cap_blocks_until_release() {
        let limiter = Arc::new(BlockingLimiter::new(1, 2));
        let lim2 = Arc::clone(&limiter);

        let guard = limiter.acquire().unwrap();

        let handle = thread::spawn(move || {
            let g = lim2.acquire().unwrap();
            assert!(g.queue_wait > Duration::ZERO);
        });

        thread::sleep(Duration::from_millis(50));
        assert_eq!(limiter.stats().queued, 1);
        drop(guard);
        handle.join().unwrap();
        assert_eq!(limiter.stats().inflight, 0);
    }

    #[test]
    fn queue_cap_zero_rejects_when_full() {
        let limiter = BlockingLimiter::new(1, 0);
        let _g = limiter.acquire().unwrap();
        let result = limiter.acquire();
        assert!(result.is_err());
        match result.unwrap_err() {
            FsError::Errno(e) => assert_eq!(e, libc::EAGAIN),
            other => panic!("expected EAGAIN, got: {other:?}"),
        }
    }

    #[test]
    fn guard_drop_releases_capacity() {
        let limiter = BlockingLimiter::new(2, 0);
        for _ in 0..100 {
            let _g = limiter.acquire().unwrap();
        }
        assert_eq!(limiter.stats().inflight, 0);
    }

    #[test]
    fn limiter_does_not_underflow() {
        let limiter = BlockingLimiter::new(4, 0);
        for _ in 0..50 {
            let _g = limiter.acquire().unwrap();
        }
        let stats = limiter.stats();
        assert_eq!(stats.inflight, 0);
        assert_eq!(stats.total_acquired, 50);
    }

    #[test]
    fn queue_wait_is_recorded() {
        let limiter = Arc::new(BlockingLimiter::new(1, 4));
        let lim2 = Arc::clone(&limiter);

        let guard = limiter.acquire().unwrap();

        let handle = thread::spawn(move || {
            let g = lim2.acquire().unwrap();
            assert!(
                g.queue_wait > Duration::from_millis(40),
                "queue_wait was {:?}",
                g.queue_wait
            );
        });

        // Hold the slot for 50ms.
        thread::sleep(Duration::from_millis(50));
        drop(guard);
        handle.join().unwrap();

        let stats = limiter.stats();
        assert!(stats.total_queue_wait >= Duration::from_millis(40));
    }

    #[test]
    fn cap_zero_always_rejects() {
        let limiter = BlockingLimiter::new(0, 10);
        for _ in 0..5 {
            let result = limiter.acquire();
            assert!(result.is_err());
            match result.unwrap_err() {
                FsError::Errno(e) => assert_eq!(e, libc::EAGAIN),
                other => panic!("expected EAGAIN, got: {other:?}"),
            }
        }
        let stats = limiter.stats();
        assert_eq!(stats.total_rejected, 5);
        assert_eq!(stats.total_acquired, 0);
    }

    #[test]
    fn concurrent_threads_respect_capacity() {
        let limiter = Arc::new(BlockingLimiter::new(4, 16));
        let active_count = Arc::new(Mutex::new(0usize));
        let max_seen = Arc::new(Mutex::new(0usize));

        let mut handles = Vec::new();
        for _ in 0..20 {
            let lim = Arc::clone(&limiter);
            let active = Arc::clone(&active_count);
            let max = Arc::clone(&max_seen);
            handles.push(thread::spawn(move || {
                let _g = lim.acquire().unwrap();
                {
                    let mut a = active.lock();
                    *a += 1;
                    let mut m = max.lock();
                    if *a > *m {
                        *m = *a;
                    }
                }
                thread::sleep(Duration::from_millis(10));
                {
                    let mut a = active.lock();
                    *a -= 1;
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let max_concurrent = *max_seen.lock();
        assert!(max_concurrent <= 4, "max concurrent was {max_concurrent}");
        assert_eq!(limiter.stats().total_acquired, 20);
    }

    #[test]
    fn stats_tracks_rejections() {
        let limiter = BlockingLimiter::new(1, 0);
        let _g = limiter.acquire().unwrap();

        for _ in 0..5 {
            let _ = limiter.acquire();
        }

        let stats = limiter.stats();
        assert_eq!(stats.total_acquired, 1);
        assert_eq!(stats.total_rejected, 5);
    }
}
