//! Trait seam for reachability probing.
//!
//! Production: `Win32Probe` (added in Task 4) runs `GetFileAttributesW` on a
//! worker-spawned helper thread, joins via `recv_timeout`. Tests inject
//! `test_mocks::MockProbe`.

/// Synchronous probe — implementations bound their own runtime; the
/// caller (worker thread) does not impose an additional timeout.
pub trait ReachabilityProbe: Send + Sync {
    /// Returns `true` if `root` is reachable, `false` on timeout or any error.
    fn probe(&self, root: &str) -> bool;
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
