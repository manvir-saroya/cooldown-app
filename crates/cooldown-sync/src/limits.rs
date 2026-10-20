//! Timeouts and abuse limits. The defaults are the numbers in the protocol document (section 6
//! and the timers in sections 2.5, 2.7 and 3.3); tests override individual fields.

use std::time::Duration;

/// The default TCP port (a placeholder in the protocol document).
pub const DEFAULT_PORT: u16 = 53320;
/// Most paired phones a PC keeps.
pub const MAX_PEERS: usize = 8;
/// A session closes after this many messages in either direction, long before a nonce could wrap.
pub const MAX_SESSION_MESSAGES: u64 = 1 << 32;

/// Every tunable. Construct with [`Limits::default`] and change fields in tests.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Unauthenticated connections the server holds at once (all peers).
    pub max_unauthenticated: usize,
    /// Unauthenticated connections per source IP.
    pub max_unauthenticated_per_ip: usize,
    /// Time to receive the 24-byte `ClientHello` after accept.
    pub hello_deadline: Duration,
    /// Time to finish the handshake (through message 2) after accept.
    pub handshake_deadline: Duration,
    /// Time for the phone's first transport message (HELLO) after message 2.
    pub first_message_deadline: Duration,
    /// Handshakes per minute per IP (token bucket); the next gets `RATE_LIMITED`.
    pub handshakes_per_minute_per_ip: u32,
    /// Handshakes per minute across all IPs.
    pub handshakes_per_minute_total: u32,
    /// The pairing window stays open this long without a successful message 1.
    pub pairing_window: Duration,
    /// Failed PAIR handshakes that burn the window.
    pub pair_failures_per_window: u8,
    /// The user's time to confirm a pairing once the code is shown.
    pub confirm_timeout: Duration,
    /// How long the phone waits for `PAIR_OK` (the PC's confirm timeout plus slack).
    pub pair_ok_timeout: Duration,
    /// Send a PING after this long without receiving anything.
    pub ping_after: Duration,
    /// Close after this long without receiving anything.
    pub silence_timeout: Duration,
    /// Shortest gap between two SYNC pushes to one phone.
    pub min_push_interval: Duration,
    /// Close if a SYNC stays unacknowledged this long.
    pub ack_timeout: Duration,
    /// Close and re-handshake after this long (there is no in-session rekey).
    pub session_lifetime: Duration,
    /// Give up on a stalled write after this long.
    pub write_timeout: Duration,
    /// Stagger between parallel connection attempts to a QR's addresses.
    pub connect_stagger: Duration,
    /// Time for one TCP connect attempt.
    pub connect_timeout: Duration,
    /// Clock offsets above this many seconds raise a warning (display only).
    pub clock_skew_warn_secs: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_unauthenticated: 32,
            max_unauthenticated_per_ip: 4,
            hello_deadline: Duration::from_secs(2),
            handshake_deadline: Duration::from_secs(5),
            first_message_deadline: Duration::from_secs(10),
            handshakes_per_minute_per_ip: 10,
            handshakes_per_minute_total: 60,
            pairing_window: Duration::from_secs(120),
            pair_failures_per_window: 5,
            confirm_timeout: Duration::from_secs(60),
            pair_ok_timeout: Duration::from_secs(75),
            ping_after: Duration::from_secs(30),
            silence_timeout: Duration::from_secs(90),
            min_push_interval: Duration::from_secs(2),
            ack_timeout: Duration::from_secs(30),
            session_lifetime: Duration::from_secs(24 * 60 * 60),
            write_timeout: Duration::from_secs(10),
            connect_stagger: Duration::from_millis(250),
            connect_timeout: Duration::from_secs(5),
            clock_skew_warn_secs: 120,
        }
    }
}

impl Limits {
    /// A copy with every timer that drives a loop raised to at least one millisecond, so a
    /// misconfigured zero cannot turn a session loop into a busy loop. (`min_push_interval` and
    /// `connect_stagger` may legitimately be zero.)
    pub fn normalized(mut self) -> Self {
        let floor = Duration::from_millis(1);
        for timer in [
            &mut self.hello_deadline,
            &mut self.handshake_deadline,
            &mut self.first_message_deadline,
            &mut self.pairing_window,
            &mut self.confirm_timeout,
            &mut self.pair_ok_timeout,
            &mut self.ping_after,
            &mut self.silence_timeout,
            &mut self.ack_timeout,
            &mut self.session_lifetime,
            &mut self.write_timeout,
            &mut self.connect_timeout,
        ] {
            *timer = (*timer).max(floor);
        }
        self.pair_failures_per_window = self.pair_failures_per_window.max(1);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_numbers() {
        let l = Limits::default();
        assert_eq!(
            (l.max_unauthenticated, l.max_unauthenticated_per_ip),
            (32, 4)
        );
        assert_eq!(l.hello_deadline, Duration::from_secs(2));
        assert_eq!(l.handshake_deadline, Duration::from_secs(5));
        assert_eq!(l.first_message_deadline, Duration::from_secs(10));
        assert_eq!(
            (
                l.handshakes_per_minute_per_ip,
                l.handshakes_per_minute_total
            ),
            (10, 60)
        );
        assert_eq!(l.pairing_window, Duration::from_secs(120));
        assert_eq!(l.pair_failures_per_window, 5);
        assert_eq!(l.confirm_timeout, Duration::from_secs(60));
        assert_eq!(l.ping_after, Duration::from_secs(30));
        assert_eq!(l.silence_timeout, Duration::from_secs(90));
        assert_eq!(l.min_push_interval, Duration::from_secs(2));
        assert_eq!(l.session_lifetime, Duration::from_secs(24 * 3600));
        assert_eq!(MAX_PEERS, 8);
        assert_eq!(MAX_SESSION_MESSAGES, 1 << 32);
    }

    #[test]
    fn zero_timers_are_raised() {
        let n = Limits {
            ping_after: Duration::ZERO,
            silence_timeout: Duration::ZERO,
            pair_failures_per_window: 0,
            ..Limits::default()
        }
        .normalized();
        assert!(n.ping_after > Duration::ZERO && n.silence_timeout > Duration::ZERO);
        assert_eq!(n.pair_failures_per_window, 1);
    }
}
