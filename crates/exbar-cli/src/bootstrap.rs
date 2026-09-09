//! Deferred toolbar creation when Explorer isn't ready yet.
//!
//! [`crate::explorer::check_explorer_ready`] gates toolbar creation on the
//! existence of Explorer's `Microsoft.UI.Content.DesktopChildSiteBridge`
//! child. On a cold first Explorer launch — the common case right after
//! login, when the WinUI 3 stack has never been paged in — that child does
//! not exist yet at the moment `EVENT_SYSTEM_FOREGROUND` fires. The probe
//! fails, and before this module the sole creation attempt was simply
//! abandoned: no toolbar until some *later* foreground event happened to
//! arrive with Explorer warm, which is why minimize-then-restore
//! "fixed" it.
//!
//! This module re-probes on a bounded schedule instead. The policy half
//! ([`next_action`], [`should_abandon`]) is pure and unit-tested; the Win32
//! half owns a thread timer whose `WM_TIMER` is delivered to
//! `retry_timer_proc` by the existing `DispatchMessageW` pump. A thread
//! timer (`SetTimer` with a null HWND) is used deliberately: the toolbar
//! window does not exist yet, so there is no window to hang a timer on.

use std::sync::Mutex;

use windows::Win32::Foundation::HWND;

// ── Policy constants ─────────────────────────────────────────────────────────

/// How long to wait between readiness probes.
pub const RETRY_INTERVAL_MS: u32 = 250;

/// Probe budget. 40 × 250 ms ≈ 10 s, comfortably longer than a cold
/// Explorer XAML init while still bounded — a pending retry that never
/// succeeds must not poll for the life of the process.
pub const MAX_ATTEMPTS: u32 = 40;

// ── Pure policy ──────────────────────────────────────────────────────────────

/// What the adapter should do after one readiness probe.
#[derive(Debug, PartialEq, Eq)]
pub enum RetryAction {
    /// Explorer is ready — create the toolbar and stop probing.
    Create,
    /// Not ready; probe again. `attempt` is the number of probes now spent.
    Retry { attempt: u32 },
    /// Budget exhausted — stop probing without creating anything.
    GiveUp,
}

/// Pure: decide what to do after a probe.
///
/// `ready` is the probe result, `attempts_so_far` the number of probes
/// already spent (0 on the first call), `max_attempts` the budget.
/// Readiness always wins, even past the budget, so a probe that succeeds
/// on the final tick still creates the toolbar.
pub fn next_action(ready: bool, attempts_so_far: u32, max_attempts: u32) -> RetryAction {
    if ready {
        return RetryAction::Create;
    }
    let attempt = attempts_so_far.saturating_add(1);
    if attempt >= max_attempts {
        return RetryAction::GiveUp;
    }
    RetryAction::Retry { attempt }
}

/// Pure: should a pending bootstrap be dropped without creating a toolbar?
///
/// Abandon when a toolbar already exists (some other path won the race),
/// when the Explorer window we were waiting on has been destroyed, or when
/// the user has switched away to an unrelated app. The last case matters:
/// creating a toolbar then would flash it over whatever is now in front.
/// Dropping the attempt is safe because any later Explorer foreground
/// event re-enters the same path.
pub fn should_abandon(
    toolbar_exists: bool,
    target_alive: bool,
    foreground_is_explorer_related: bool,
) -> bool {
    toolbar_exists || !target_alive || !foreground_is_explorer_related
}

// ── Win32 adapter ────────────────────────────────────────────────────────────

/// The single in-flight bootstrap attempt, if any.
struct Pending {
    /// `CabinetWClass` HWND we are waiting on, as a raw `isize` (HWND is
    /// `!Send`; the single-threaded pump invariant makes this safe).
    cabinet: isize,
    attempts: u32,
    timer_id: usize,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

/// Arm (or retarget) the deferred-creation retry for `cabinet`.
///
/// Called when a `CabinetWClass` takes foreground but its XAML bridge is
/// not up yet. Re-arming for a different Explorer window retargets the
/// existing timer and resets the budget rather than stacking timers.
pub fn schedule_retry(cabinet: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::SetTimer;

    let mut pending = PENDING.lock().unwrap();
    if let Some(p) = pending.as_mut() {
        if p.cabinet != cabinet.0 as isize {
            log::debug!("bootstrap: retarget pending retry to explorer={cabinet:?}");
            p.cabinet = cabinet.0 as isize;
            p.attempts = 0;
        }
        return;
    }

    // SAFETY: a null HWND asks Win32 for a thread timer; the returned id is
    // ours to kill. WM_TIMER for it carries our TIMERPROC in lParam, which
    // DispatchMessageW invokes directly.
    let timer_id = unsafe { SetTimer(None, 0, RETRY_INTERVAL_MS, Some(retry_timer_proc)) };
    if timer_id == 0 {
        log::warn!("bootstrap: SetTimer failed; no deferred toolbar creation");
        return;
    }
    log::info!(
        "bootstrap: explorer={cabinet:?} not ready, probing every {RETRY_INTERVAL_MS}ms (budget {MAX_ATTEMPTS})"
    );
    *pending = Some(Pending {
        cabinet: cabinet.0 as isize,
        attempts: 0,
        timer_id,
    });
}

/// Kill the retry timer and forget the pending attempt. Idempotent.
fn cancel_locked(pending: &mut Option<Pending>) {
    use windows::Win32::UI::WindowsAndMessaging::KillTimer;
    if let Some(p) = pending.take() {
        // SAFETY: `timer_id` came from our own SetTimer(None, ...) call.
        unsafe {
            let _ = KillTimer(None, p.timer_id);
        }
    }
}

/// True if the foreground window means "Explorer is still what the user is
/// looking at" — the same predicate the foreground handler uses to decide a
/// show is genuine.
fn foreground_is_explorer_related(cabinet: HWND) -> bool {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
    let fg = unsafe { GetForegroundWindow() };
    fg == cabinet
        || crate::explorer::get_class_name(fg) == "CabinetWClass"
        || crate::visibility::hwnd_in_explorer_process(fg)
        || crate::visibility::hwnd_in_our_process(fg)
}

/// Thread-timer callback: re-probe Explorer readiness and create the
/// toolbar as soon as the probe succeeds.
///
/// # Safety
/// Registered as a `TIMERPROC`; Win32 guarantees the signature. Runs on the
/// message-pump thread via `DispatchMessageW`, the same single-threaded
/// invariant the rest of the toolbar code relies on.
unsafe extern "system" fn retry_timer_proc(_hwnd: HWND, _msg: u32, _id: usize, _time: u32) {
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    let mut pending = PENDING.lock().unwrap();
    let Some(p) = pending.as_mut() else {
        return;
    };
    let cabinet = HWND(p.cabinet as *mut _);

    let toolbar_exists = crate::visibility::get_global_toolbar_hwnd().is_some();
    let target_alive = unsafe { IsWindow(Some(cabinet)).as_bool() };
    if should_abandon(
        toolbar_exists,
        target_alive,
        foreground_is_explorer_related(cabinet),
    ) {
        log::debug!(
            "bootstrap: abandoning retry for {cabinet:?} (toolbar={toolbar_exists} alive={target_alive})"
        );
        cancel_locked(&mut pending);
        return;
    }

    let ready = crate::explorer::check_explorer_ready(cabinet);
    match next_action(ready.is_some(), p.attempts, MAX_ATTEMPTS) {
        RetryAction::Create => {
            let info = ready.expect("next_action returns Create only when the probe succeeded");
            log::info!(
                "bootstrap: explorer={cabinet:?} ready after {} retries — creating toolbar",
                p.attempts
            );
            cancel_locked(&mut pending);
            // Release the lock before creating: WM_CREATE runs synchronously
            // inside CreateWindowExW and must not re-enter a held PENDING lock.
            drop(pending);
            let hinst = crate::lifecycle::exe_hinstance();
            let _ = crate::lifecycle::create_toolbar(info.cabinet_hwnd, &info.default_pos, hinst);
        }
        RetryAction::Retry { attempt } => {
            p.attempts = attempt;
        }
        RetryAction::GiveUp => {
            log::warn!(
                "bootstrap: explorer={cabinet:?} never became ready after {MAX_ATTEMPTS} probes — giving up"
            );
            cancel_locked(&mut pending);
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_probe_creates_immediately() {
        assert_eq!(next_action(true, 0, MAX_ATTEMPTS), RetryAction::Create);
    }

    #[test]
    fn ready_probe_wins_even_on_the_last_attempt() {
        // Readiness must not be discarded just because the budget is spent.
        assert_eq!(
            next_action(true, MAX_ATTEMPTS - 1, MAX_ATTEMPTS),
            RetryAction::Create
        );
    }

    #[test]
    fn first_failed_probe_schedules_a_retry() {
        assert_eq!(
            next_action(false, 0, MAX_ATTEMPTS),
            RetryAction::Retry { attempt: 1 }
        );
    }

    #[test]
    fn attempts_count_up_toward_the_budget() {
        assert_eq!(
            next_action(false, 5, MAX_ATTEMPTS),
            RetryAction::Retry { attempt: 6 }
        );
    }

    #[test]
    fn last_probe_in_the_budget_gives_up() {
        assert_eq!(
            next_action(false, MAX_ATTEMPTS - 1, MAX_ATTEMPTS),
            RetryAction::GiveUp
        );
    }

    #[test]
    fn zero_budget_never_retries() {
        assert_eq!(next_action(false, 0, 0), RetryAction::GiveUp);
    }

    #[test]
    fn attempts_saturate_rather_than_overflow() {
        assert_eq!(
            next_action(false, u32::MAX, MAX_ATTEMPTS),
            RetryAction::GiveUp
        );
    }

    #[test]
    fn healthy_pending_attempt_is_kept() {
        assert!(!should_abandon(false, true, true));
    }

    #[test]
    fn existing_toolbar_abandons_the_attempt() {
        assert!(should_abandon(true, true, true));
    }

    #[test]
    fn destroyed_explorer_abandons_the_attempt() {
        assert!(should_abandon(false, false, true));
    }

    #[test]
    fn switching_to_a_foreign_app_abandons_the_attempt() {
        assert!(should_abandon(false, true, false));
    }
}
