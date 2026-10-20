//! Time source abstraction so schedulers and the store are testable without real waiting.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use time::OffsetDateTime;
use tokio::sync::oneshot;

/// A source of wall-clock time and of sleeping.
///
/// `sleep` must be cancel-safe: dropping the returned future abandons the wait without side
/// effects.
#[async_trait]
pub trait Clock: Send + Sync + 'static {
    /// The current UTC time.
    fn now(&self) -> OffsetDateTime;

    /// Wait for `duration` of this clock's time.
    async fn sleep(&self, duration: Duration);
}

/// The real clock: `OffsetDateTime::now_utc` and `tokio::time::sleep`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

#[async_trait]
impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }

    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}

struct Waiter {
    deadline: OffsetDateTime,
    wake: oneshot::Sender<()>,
}

struct FakeState {
    now: OffsetDateTime,
    waiters: Vec<Waiter>,
}

/// A manually driven clock for tests.
///
/// Time only moves when [`FakeClock::advance`] or [`FakeClock::set`] is called. `advance` steps
/// through every sleeper's deadline in order and lets woken tasks run (and register their next
/// sleep) before moving on, so a task that sleeps repeatedly sees each tick inside one `advance`.
/// Designed for the single-threaded `#[tokio::test]` runtime.
pub struct FakeClock {
    state: Mutex<FakeState>,
}

impl FakeClock {
    /// A fake clock starting at `start`.
    pub fn new(start: OffsetDateTime) -> Arc<Self> {
        Arc::new(FakeClock {
            state: Mutex::new(FakeState {
                now: start,
                waiters: Vec::new(),
            }),
        })
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Number of tasks currently sleeping on this clock (abandoned sleeps are not counted).
    pub fn sleepers(&self) -> usize {
        let mut state = self.lock();
        state.waiters.retain(|w| !w.wake.is_closed());
        state.waiters.len()
    }

    /// Let spawned tasks run until they block again.
    pub async fn settle() {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
    }

    /// Move time forward by `by`, waking sleepers whose deadline passes along the way.
    pub async fn advance(&self, by: Duration) {
        let target = self.lock().now + by;
        self.advance_to(target).await;
    }

    /// Move time to `target` (never backwards), waking sleepers along the way.
    pub async fn advance_to(&self, target: OffsetDateTime) {
        // Let anything that is already runnable register its sleep first.
        Self::settle().await;
        loop {
            let next = {
                let mut state = self.lock();
                state.waiters.retain(|w| !w.wake.is_closed());
                let earliest = state
                    .waiters
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| w.deadline <= target)
                    .min_by_key(|(_, w)| w.deadline)
                    .map(|(index, _)| index);
                earliest.map(|index| {
                    let waiter = state.waiters.remove(index);
                    if waiter.deadline > state.now {
                        state.now = waiter.deadline;
                    }
                    waiter
                })
            };
            match next {
                Some(waiter) => {
                    let _ = waiter.wake.send(());
                    Self::settle().await;
                }
                None => break,
            }
        }
        let mut state = self.lock();
        if target > state.now {
            state.now = target;
        }
    }

    /// Jump straight to `to` without waking anyone early; sleepers due by then wake on the next
    /// `advance`. Use for "the machine slept for hours" scenarios.
    pub fn jump_to(&self, to: OffsetDateTime) {
        let mut state = self.lock();
        if to > state.now {
            state.now = to;
        }
    }
}

#[async_trait]
impl Clock for FakeClock {
    fn now(&self) -> OffsetDateTime {
        self.lock().now
    }

    async fn sleep(&self, duration: Duration) {
        let receiver = {
            let mut state = self.lock();
            if duration.is_zero() {
                return;
            }
            let (wake, receiver) = oneshot::channel();
            let deadline = state.now + duration;
            state.waiters.push(Waiter { deadline, wake });
            receiver
        };
        let _ = receiver.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use time::macros::datetime;

    #[tokio::test]
    async fn sleepers_wake_in_deadline_order_and_time_moves_to_each_deadline() {
        let clock = FakeClock::new(datetime!(2026-10-10 12:00 UTC));
        let log = Arc::new(Mutex::new(Vec::new()));
        for secs in [30u64, 10, 20] {
            let clock = clock.clone();
            let log = log.clone();
            tokio::spawn(async move {
                clock.sleep(Duration::from_secs(secs)).await;
                log.lock().unwrap().push((secs, clock.now()));
            });
        }
        FakeClock::settle().await;
        assert_eq!(clock.sleepers(), 3);
        clock.advance(Duration::from_secs(25)).await;
        {
            let log = log.lock().unwrap();
            assert_eq!(
                *log,
                vec![
                    (10, datetime!(2026-10-10 12:00:10 UTC)),
                    (20, datetime!(2026-10-10 12:00:20 UTC)),
                ]
            );
        }
        assert_eq!(clock.now(), datetime!(2026-10-10 12:00:25 UTC));
        clock.advance(Duration::from_secs(5)).await;
        assert_eq!(log.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_repeating_task_ticks_inside_one_advance() {
        let clock = FakeClock::new(datetime!(2026-10-10 12:00 UTC));
        let ticks = Arc::new(AtomicU32::new(0));
        {
            let clock = clock.clone();
            let ticks = ticks.clone();
            tokio::spawn(async move {
                loop {
                    clock.sleep(Duration::from_secs(60)).await;
                    ticks.fetch_add(1, Ordering::SeqCst);
                }
            });
        }
        FakeClock::settle().await;
        clock.advance(Duration::from_secs(60 * 5 + 30)).await;
        assert_eq!(ticks.load(Ordering::SeqCst), 5);
    }

    #[tokio::test]
    async fn dropped_sleeps_are_not_counted_and_zero_sleep_returns() {
        let clock = FakeClock::new(datetime!(2026-10-10 12:00 UTC));
        clock.sleep(Duration::ZERO).await;
        let handle = {
            let clock = clock.clone();
            tokio::spawn(async move { clock.sleep(Duration::from_secs(5)).await })
        };
        FakeClock::settle().await;
        assert_eq!(clock.sleepers(), 1);
        handle.abort();
        FakeClock::settle().await;
        assert_eq!(clock.sleepers(), 0);
    }

    #[tokio::test]
    async fn time_never_goes_backwards() {
        let clock = FakeClock::new(datetime!(2026-10-10 12:00 UTC));
        clock.advance_to(datetime!(2026-10-10 11:00 UTC)).await;
        assert_eq!(clock.now(), datetime!(2026-10-10 12:00 UTC));
        clock.jump_to(datetime!(2026-10-10 11:00 UTC));
        assert_eq!(clock.now(), datetime!(2026-10-10 12:00 UTC));
        clock.jump_to(datetime!(2026-10-10 15:00 UTC));
        assert_eq!(clock.now(), datetime!(2026-10-10 15:00 UTC));
    }

    #[tokio::test]
    async fn system_clock_reports_utc_and_sleeps() {
        let clock = SystemClock;
        let before = OffsetDateTime::now_utc();
        clock.sleep(Duration::from_millis(5)).await;
        assert!(clock.now() >= before);
        assert!(clock.now().offset().is_utc());
    }
}
