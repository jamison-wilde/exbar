//! Time-source trait seam so tests can control "now" deterministically.

/// Trait seam for getting the current wall-clock time in unix milliseconds.
pub trait Clock: Send + Sync {
    fn now_unix_ms(&self) -> u64;
}

/// Production clock — real system time.
#[derive(Default)]
pub struct SystemClock;

impl SystemClock {
    pub fn new() -> Self {
        Self
    }
}

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

#[cfg(test)]
pub(crate) mod test_mocks {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Thread-safe mock clock. `new(initial)` sets starting value; `set` and
    /// `advance` mutate; `now_unix_ms` reads.
    #[derive(Default)]
    pub struct MockClock {
        now: AtomicU64,
    }

    impl MockClock {
        pub fn new(initial: u64) -> Self {
            Self {
                now: AtomicU64::new(initial),
            }
        }
        pub fn set(&self, v: u64) {
            self.now.store(v, Ordering::SeqCst);
        }
        pub fn advance(&self, dt: u64) {
            self.now.fetch_add(dt, Ordering::SeqCst);
        }
    }

    impl Clock for MockClock {
        fn now_unix_ms(&self) -> u64 {
            self.now.load(Ordering::SeqCst)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_returns_nonzero() {
        let c = SystemClock::new();
        assert!(c.now_unix_ms() > 0);
    }

    #[test]
    fn mock_clock_set_and_advance() {
        let c = test_mocks::MockClock::new(1000);
        assert_eq!(c.now_unix_ms(), 1000);
        c.advance(500);
        assert_eq!(c.now_unix_ms(), 1500);
        c.set(42);
        assert_eq!(c.now_unix_ms(), 42);
    }
}
