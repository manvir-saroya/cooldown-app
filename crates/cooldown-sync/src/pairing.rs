//! The PC-side pairing window: single use, time limited, burned after repeated failures.
//!
//! State machine (all times on the monotonic clock):
//!
//! ```text
//!            open()                    admit()+try_lock()
//!  Closed ------------> Open(unlocked) ------------------> Open(locked)
//!    ^                     |  expiry / 5 failures /             |
//!    |                     |  close()                           |  complete() / burn()
//!    +---------------------+------------------------------------+
//! ```
//!
//! The PSK lives only inside `Open` and is wiped when the window closes for any reason. Locking
//! happens on the first *successful* message 1; after that the window is no longer subject to the
//! 120 s limit (the user's confirmation has its own deadline) and later attempts get `BUSY`.

use std::time::Duration;

use subtle::ConstantTimeEq;
use zeroize::Zeroizing;

/// Why a window closed. Reported in events; never carries key material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowClosed {
    /// The 120 s elapsed with no successful attempt.
    Expired,
    /// Too many failed handshakes: the PSK is burned.
    Burned,
    /// The user hid the QR code.
    Hidden,
    /// A phone was paired.
    Completed,
    /// The user rejected the pairing, it timed out, or the phone vanished before confirmation.
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Admit {
    /// Proceed; the number identifies this window for later bookkeeping.
    Proceed(u64),
    /// No window, an expired one, or a different `pair_id`. Reported as `NOT_PAIRING`.
    NotPairing,
    /// A pairing is already in progress.
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockResult {
    Locked,
    NotPairing,
    Busy,
}

struct Open {
    generation: u64,
    pair_id: [u8; 16],
    psk: Zeroizing<[u8; 32]>,
    opened_at: Duration,
    failures: u8,
    locked: bool,
}

/// One pairing window at a time.
pub(crate) struct PairingWindow {
    open: Option<Open>,
    next_generation: u64,
    ttl: Duration,
    max_failures: u8,
}

impl PairingWindow {
    pub(crate) fn new(ttl: Duration, max_failures: u8) -> Self {
        PairingWindow {
            open: None,
            next_generation: 1,
            ttl,
            max_failures,
        }
    }

    /// Close the window if it has expired (an unlocked window only).
    fn expire(&mut self, now: Duration) -> Option<WindowClosed> {
        let expired = self
            .open
            .as_ref()
            .is_some_and(|w| !w.locked && now.saturating_sub(w.opened_at) >= self.ttl);
        if expired {
            self.open = None;
            Some(WindowClosed::Expired)
        } else {
            None
        }
    }

    /// Open a window. Returns its generation, or `None` if one is already open (and unexpired).
    pub(crate) fn open(
        &mut self,
        pair_id: [u8; 16],
        psk: Zeroizing<[u8; 32]>,
        now: Duration,
    ) -> Option<u64> {
        self.expire(now);
        if self.open.is_some() {
            return None;
        }
        let generation = self.next_generation;
        self.next_generation += 1;
        self.open = Some(Open {
            generation,
            pair_id,
            psk,
            opened_at: now,
            failures: 0,
            locked: false,
        });
        Some(generation)
    }

    /// Check an incoming PAIR hello before any DH.
    pub(crate) fn admit(&mut self, pair_id: &[u8; 16], now: Duration) -> Admit {
        self.expire(now);
        match &self.open {
            None => Admit::NotPairing,
            Some(w) if !bool::from(w.pair_id.ct_eq(pair_id)) => Admit::NotPairing,
            Some(w) if w.locked => Admit::Busy,
            Some(w) => Admit::Proceed(w.generation),
        }
    }

    /// A copy of the PSK for one handshake attempt, wiped when dropped.
    pub(crate) fn psk(&self, generation: u64) -> Option<Zeroizing<[u8; 32]>> {
        self.open
            .as_ref()
            .filter(|w| w.generation == generation && !w.locked)
            .map(|w| Zeroizing::new(*w.psk))
    }

    /// Count a failed attempt. Returns `true` when this burned the window.
    pub(crate) fn record_failure(&mut self, generation: u64) -> bool {
        let Some(w) = self.open.as_mut().filter(|w| w.generation == generation) else {
            return false;
        };
        if w.locked {
            return false;
        }
        w.failures = w.failures.saturating_add(1);
        if w.failures >= self.max_failures {
            self.open = None;
            return true;
        }
        false
    }

    /// Lock the window on the first successful message 1.
    pub(crate) fn try_lock(&mut self, generation: u64, now: Duration) -> LockResult {
        self.expire(now);
        match self.open.as_mut() {
            Some(w) if w.generation == generation && !w.locked => {
                w.locked = true;
                LockResult::Locked
            }
            Some(w) if w.generation == generation => LockResult::Busy,
            Some(_) | None => LockResult::NotPairing,
        }
    }

    /// Close the window of `generation` (completed, burned, rejected, hidden). Returns whether
    /// that window was open.
    pub(crate) fn close(&mut self, generation: u64) -> bool {
        if self
            .open
            .as_ref()
            .is_some_and(|w| w.generation == generation)
        {
            self.open = None;
            true
        } else {
            false
        }
    }

    /// Close whatever window is open and unlocked (the Hide button). Returns its generation.
    pub(crate) fn hide(&mut self) -> Option<u64> {
        match &self.open {
            Some(w) if !w.locked => {
                let generation = w.generation;
                self.open = None;
                Some(generation)
            }
            _ => None,
        }
    }

    /// Expire the window of `generation` if due. For the background expiry task.
    pub(crate) fn expire_generation(&mut self, generation: u64, now: Duration) -> bool {
        if self
            .open
            .as_ref()
            .is_some_and(|w| w.generation == generation)
        {
            return self.expire(now).is_some();
        }
        false
    }

    /// The generation of the open, locked window (a pairing awaiting confirmation).
    #[cfg(test)]
    pub(crate) fn locked_generation(&self) -> Option<u64> {
        self.open
            .as_ref()
            .filter(|w| w.locked)
            .map(|w| w.generation)
    }

    /// Whether any window is open.
    #[cfg(test)]
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(120);

    fn window() -> (PairingWindow, u64) {
        let mut w = PairingWindow::new(TTL, 5);
        let generation = w
            .open([1; 16], Zeroizing::new([2; 32]), Duration::ZERO)
            .unwrap();
        (w, generation)
    }

    #[test]
    fn admits_only_the_matching_pair_id_while_open() {
        let (mut w, generation) = window();
        assert_eq!(
            w.admit(&[1; 16], Duration::from_secs(1)),
            Admit::Proceed(generation)
        );
        assert_eq!(w.admit(&[9; 16], Duration::from_secs(1)), Admit::NotPairing);
        let mut closed = PairingWindow::new(TTL, 5);
        assert_eq!(closed.admit(&[1; 16], Duration::ZERO), Admit::NotPairing);
    }

    #[test]
    fn expires_after_120_seconds() {
        let (mut w, _) = window();
        assert!(matches!(
            w.admit(&[1; 16], Duration::from_secs(119)),
            Admit::Proceed(_)
        ));
        assert_eq!(
            w.admit(&[1; 16], Duration::from_secs(120)),
            Admit::NotPairing
        );
        assert!(!w.is_open());
    }

    #[test]
    fn five_failures_burn_the_window() {
        let (mut w, generation) = window();
        for _ in 0..4 {
            assert!(!w.record_failure(generation));
            assert!(matches!(
                w.admit(&[1; 16], Duration::ZERO),
                Admit::Proceed(_)
            ));
        }
        assert!(w.record_failure(generation));
        assert_eq!(w.admit(&[1; 16], Duration::ZERO), Admit::NotPairing);
        assert!(w.psk(generation).is_none());
    }

    #[test]
    fn a_wrong_pair_id_is_not_a_failure() {
        let (mut w, generation) = window();
        for _ in 0..50 {
            assert_eq!(w.admit(&[7; 16], Duration::ZERO), Admit::NotPairing);
        }
        assert!(matches!(w.admit(&[1; 16], Duration::ZERO), Admit::Proceed(g) if g == generation));
    }

    #[test]
    fn locking_makes_later_attempts_busy_and_lifts_the_expiry() {
        let (mut w, generation) = window();
        assert_eq!(
            w.try_lock(generation, Duration::from_secs(10)),
            LockResult::Locked
        );
        assert_eq!(w.admit(&[1; 16], Duration::from_secs(11)), Admit::Busy);
        assert_eq!(
            w.try_lock(generation, Duration::from_secs(11)),
            LockResult::Busy
        );
        // The user may take their 60 s even past the 120 s mark.
        assert_eq!(w.admit(&[1; 16], Duration::from_secs(500)), Admit::Busy);
        assert_eq!(w.locked_generation(), Some(generation));
        assert!(
            w.psk(generation).is_none(),
            "PSK is not handed out once locked"
        );
        assert!(w.close(generation));
        assert_eq!(
            w.admit(&[1; 16], Duration::from_secs(501)),
            Admit::NotPairing
        );
    }

    #[test]
    fn only_one_window_at_a_time_and_the_old_psk_does_not_survive() {
        let (mut w, generation) = window();
        assert!(w
            .open([3; 16], Zeroizing::new([4; 32]), Duration::from_secs(1))
            .is_none());
        assert!(w.close(generation));
        let second = w
            .open([3; 16], Zeroizing::new([4; 32]), Duration::from_secs(2))
            .unwrap();
        assert_ne!(second, generation);
        // Bookkeeping for the old window is inert.
        assert!(!w.record_failure(generation));
        assert!(!w.close(generation));
        assert_eq!(w.admit(&[1; 16], Duration::from_secs(3)), Admit::NotPairing);
        assert!(
            matches!(w.admit(&[3; 16], Duration::from_secs(3)), Admit::Proceed(g) if g == second)
        );
    }

    #[test]
    fn hide_only_closes_an_unlocked_window() {
        let (mut w, generation) = window();
        assert_eq!(w.hide(), Some(generation));
        assert_eq!(w.hide(), None);
        let (mut w, generation) = window();
        w.try_lock(generation, Duration::ZERO);
        assert_eq!(w.hide(), None);
    }

    #[test]
    fn expiry_task_only_affects_its_own_window() {
        let (mut w, generation) = window();
        assert!(!w.expire_generation(generation, Duration::from_secs(10)));
        assert!(w.expire_generation(generation, Duration::from_secs(120)));
        let second = w
            .open([5; 16], Zeroizing::new([6; 32]), Duration::from_secs(130))
            .unwrap();
        assert!(!w.expire_generation(generation, Duration::from_secs(400)));
        assert!(w.expire_generation(second, Duration::from_secs(400)));
    }
}
