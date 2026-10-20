//! Time sources.
//!
//! The protocol needs two different notions of time:
//!
//! * a **monotonic** clock for everything that decides security or liveness (the 120 s pairing
//!   window, rate-limit buckets, deadlines, idle timers, the 24 h session lifetime), and
//! * the **wall** clock, used only for display, the reconnect `ts` hint and the clock-offset
//!   estimate. No security decision depends on it.
//!
//! Production code uses [`SystemSyncClock`]. Tests use `cooldown_core::FakeClock`, which
//! implements [`SyncClock`] here, so a test can step 120 s in no real time.

use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use cooldown_core::FakeClock;

/// A boxed, sendable future.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The clock every time-dependent decision in this crate goes through.
pub trait SyncClock: Send + Sync + 'static {
    /// Monotonic time since an arbitrary origin. Never goes backwards.
    fn mono(&self) -> Duration;

    /// Unix wall-clock time in milliseconds. May jump; never used for security decisions.
    fn wall_ms(&self) -> i64;

    /// Sleep for `duration` of this clock's time. Dropping the future abandons the wait.
    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()>;
}

/// The real clocks: `Instant` for monotonic time, `SystemTime` for the wall clock.
#[derive(Debug, Clone, Copy)]
pub struct SystemSyncClock {
    origin: Instant,
}

impl SystemSyncClock {
    /// A clock whose monotonic origin is now.
    pub fn new() -> Self {
        SystemSyncClock {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemSyncClock {
    fn default() -> Self {
        Self::new()
    }
}

impl SyncClock for SystemSyncClock {
    fn mono(&self) -> Duration {
        self.origin.elapsed()
    }

    fn wall_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(since) => i64::try_from(since.as_millis()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

/// `FakeClock` moves only when a test advances it, which makes it monotonic by construction.
impl SyncClock for FakeClock {
    fn mono(&self) -> Duration {
        let nanos = cooldown_core::Clock::now(self).unix_timestamp_nanos();
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(0))
    }

    fn wall_ms(&self) -> i64 {
        let millis = cooldown_core::Clock::now(self).unix_timestamp_nanos() / 1_000_000;
        i64::try_from(millis).unwrap_or(i64::MAX)
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'_, ()> {
        cooldown_core::Clock::sleep(self, duration)
    }
}

/// Run `future`, giving up after `duration` of `clock` time. The timed-out future is dropped, so
/// only use it where abandoning the operation is fine (handshakes, which then close).
pub(crate) async fn with_deadline<T>(
    clock: &dyn SyncClock,
    duration: Duration,
    future: impl Future<Output = T>,
) -> Result<T, TimedOut> {
    tokio::select! {
        biased;
        value = future => Ok(value),
        () = clock.sleep(duration) => Err(TimedOut),
    }
}

/// A deadline passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimedOut;

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    #[tokio::test]
    async fn fake_clock_drives_sleep_and_mono() {
        let clock = FakeClock::new(OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1_000));
        let start = SyncClock::mono(&*clock);
        let sleeper = {
            let clock = clock.clone();
            tokio::spawn(async move { SyncClock::sleep(&*clock, Duration::from_secs(120)).await })
        };
        clock.advance(Duration::from_secs(119)).await;
        assert!(!sleeper.is_finished());
        clock.advance(Duration::from_secs(2)).await;
        sleeper.await.unwrap();
        assert_eq!(SyncClock::mono(&*clock) - start, Duration::from_secs(121));
        assert_eq!(SyncClock::wall_ms(&*clock), 1_121_000);
    }

    #[tokio::test]
    async fn system_clock_is_monotonic_and_sane() {
        let clock = SystemSyncClock::new();
        let a = clock.mono();
        clock.sleep(Duration::from_millis(5)).await;
        assert!(clock.mono() > a);
        assert!(clock.wall_ms() > 1_600_000_000_000);
    }

    #[tokio::test]
    async fn deadline_fires_on_fake_time() {
        let clock = FakeClock::new(OffsetDateTime::UNIX_EPOCH);
        let waiter = {
            let clock = clock.clone();
            tokio::spawn(async move {
                with_deadline(
                    &*clock,
                    Duration::from_secs(2),
                    std::future::pending::<()>(),
                )
                .await
            })
        };
        clock.advance(Duration::from_secs(3)).await;
        assert_eq!(waiter.await.unwrap(), Err(TimedOut));
    }
}
