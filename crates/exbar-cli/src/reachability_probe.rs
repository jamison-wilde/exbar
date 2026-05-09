//! Trait seam for reachability probing.
//!
//! Production: `Win32Probe` (added in Task 4) runs `GetFileAttributesW` on a
//! worker-spawned helper thread, joins via `recv_timeout`. Tests inject
//! `test_mocks::MockProbe`.

use std::sync::mpsc;
use std::time::Duration;

/// Default probe wall-clock budget. Probes exceeding this are treated as
/// unreachable (the spawned helper thread keeps running until the OS
/// returns; its result is then discarded).
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Synchronous probe — implementations bound their own runtime; the
/// caller (worker thread) does not impose an additional timeout.
pub trait ReachabilityProbe: Send + Sync {
    /// Returns `true` if `root` is reachable, `false` on timeout or any error.
    fn probe(&self, root: &str) -> bool;
}

/// Production probe — `GetFileAttributesW` on a fresh helper thread,
/// joined with `recv_timeout`.
pub struct Win32Probe {
    timeout: Duration,
}

impl Win32Probe {
    pub fn new() -> Self {
        Self {
            timeout: DEFAULT_PROBE_TIMEOUT,
        }
    }

    #[cfg(test)]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl Default for Win32Probe {
    fn default() -> Self {
        Self::new()
    }
}

impl ReachabilityProbe for Win32Probe {
    fn probe(&self, root: &str) -> bool {
        let (tx, rx) = mpsc::channel();
        let r = root.to_owned();
        std::thread::spawn(move || {
            let reachable = probe_blocking(&r);
            // Receiver may have been dropped on timeout — ignore SendError.
            let _ = tx.send(reachable);
        });
        rx.recv_timeout(self.timeout).unwrap_or(false)
    }
}

/// Blocking probe primitive — `GetFileAttributesW` on the root with a
/// trailing backslash (required for both `X:` and `\\srv\share`). Returns
/// true iff the call returns a non-`INVALID_FILE_ATTRIBUTES` value.
fn probe_blocking(root: &str) -> bool {
    use windows::Win32::Storage::FileSystem::{GetFileAttributesW, INVALID_FILE_ATTRIBUTES};
    let mut wide: Vec<u16> = root.encode_utf16().collect();
    if wide.last() != Some(&(b'\\' as u16)) {
        wide.push(b'\\' as u16);
    }
    wide.push(0);
    let attrs = unsafe { GetFileAttributesW(windows_core::PCWSTR(wide.as_ptr())) };
    attrs != INVALID_FILE_ATTRIBUTES
}

#[cfg(test)]
pub(crate) mod test_mocks {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::Duration;

    /// In-memory reachability probe. Test seed:
    /// - `set_result(root, true|false)` to control return value
    /// - set `delay` field directly to insert a sleep before returning
    ///   (for concurrency tests)
    #[derive(Default)]
    pub struct MockProbe {
        pub results: Mutex<HashMap<String, bool>>,
        pub delay: Mutex<Option<Duration>>,
        pub calls: Mutex<Vec<String>>,
    }

    impl MockProbe {
        pub fn set_result(&self, root: &str, reachable: bool) {
            self.results
                .lock()
                .unwrap()
                .insert(root.to_owned(), reachable);
        }
    }

    impl ReachabilityProbe for MockProbe {
        fn probe(&self, root: &str) -> bool {
            self.calls.lock().unwrap().push(root.to_owned());
            if let Some(d) = *self.delay.lock().unwrap() {
                std::thread::sleep(d);
            }
            self.results
                .lock()
                .unwrap()
                .get(root)
                .copied()
                .unwrap_or(false)
        }
    }

    #[test]
    fn mock_returns_seeded_result() {
        let p = MockProbe::default();
        p.set_result("Z:", true);
        assert!(p.probe("Z:"));
        assert!(!p.probe("Y:")); // unseeded → false
        assert_eq!(
            *p.calls.lock().unwrap(),
            vec!["Z:".to_string(), "Y:".to_string()]
        );
    }
}

#[cfg(test)]
mod win32_tests {
    use super::*;

    #[test]
    fn win32_probe_local_root_is_reachable() {
        // C:\ should always exist on a Windows host — sanity-check the primitive.
        let p = Win32Probe::new();
        assert!(p.probe("C:"));
    }

    #[test]
    fn win32_probe_nonexistent_unc_times_out_or_fails_quickly() {
        // RFC 5737 TEST-NET-1 — guaranteed non-routable. Set a 1.5s timeout so
        // the test isn't slow when SMB takes its 30s default.
        let p = Win32Probe::with_timeout(Duration::from_millis(1500));
        let start = std::time::Instant::now();
        let r = p.probe("\\\\192.0.2.1\\nope");
        let elapsed = start.elapsed();
        assert!(!r, "expected unreachable, got {r}");
        assert!(
            elapsed < Duration::from_secs(3),
            "elapsed {elapsed:?} exceeded soft cap"
        );
    }
}
