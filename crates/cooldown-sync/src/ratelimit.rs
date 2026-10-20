//! Abuse limits for unauthenticated traffic: concurrent-connection caps, handshake token buckets,
//! and a gate that keeps log volume bounded.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Most per-IP buckets tracked at once; beyond this the least recently used one is evicted (the
/// overall bucket still applies, so eviction cannot be used to exceed it).
const MAX_TRACKED_IPS: usize = 1024;

#[derive(Debug, Clone)]
struct Bucket {
    capacity: f64,
    tokens: f64,
    per_second: f64,
    last: Duration,
}

impl Bucket {
    fn per_minute(rate: u32, now: Duration) -> Self {
        let capacity = f64::from(rate);
        Bucket {
            capacity,
            tokens: capacity,
            per_second: capacity / 60.0,
            last: now,
        }
    }

    fn refill(&mut self, now: Duration) {
        let elapsed = now.saturating_sub(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
        self.last = self.last.max(now);
    }

    fn has_token(&self) -> bool {
        self.tokens >= 1.0
    }
}

/// Token buckets for handshake attempts: per source IP and overall.
#[derive(Debug)]
pub(crate) struct HandshakeLimiter {
    per_ip_rate: u32,
    per_ip: HashMap<IpAddr, Bucket>,
    total: Bucket,
}

impl HandshakeLimiter {
    pub(crate) fn new(per_ip_per_minute: u32, total_per_minute: u32, now: Duration) -> Self {
        HandshakeLimiter {
            per_ip_rate: per_ip_per_minute,
            per_ip: HashMap::new(),
            total: Bucket::per_minute(total_per_minute, now),
        }
    }

    /// Spend one token for `ip`. `false` means the handshake must get `RATE_LIMITED`. An IP that
    /// is over its own limit does not drain the overall bucket.
    pub(crate) fn admit(&mut self, ip: IpAddr, now: Duration) -> bool {
        if !self.per_ip.contains_key(&ip) && self.per_ip.len() >= MAX_TRACKED_IPS {
            self.evict(now);
        }
        let rate = self.per_ip_rate;
        let bucket = self
            .per_ip
            .entry(ip)
            .or_insert_with(|| Bucket::per_minute(rate, now));
        bucket.refill(now);
        self.total.refill(now);
        if !bucket.has_token() || !self.total.has_token() {
            return false;
        }
        bucket.tokens -= 1.0;
        self.total.tokens -= 1.0;
        true
    }

    fn evict(&mut self, now: Duration) {
        // Idle, fully refilled buckets carry no information; drop them first.
        self.per_ip.retain(|_, b| {
            let mut probe = b.clone();
            probe.refill(now);
            probe.tokens < probe.capacity
        });
        if self.per_ip.len() >= MAX_TRACKED_IPS {
            if let Some(oldest) = self
                .per_ip
                .iter()
                .min_by_key(|(_, b)| b.last)
                .map(|(ip, _)| *ip)
            {
                self.per_ip.remove(&oldest);
            }
        }
    }
}

/// Allows one beacon reply per source IP per interval (the document's "at most once per IP every
/// 2 s"). The table is bounded: when it is full of entries that are all still inside their
/// interval, new sources are refused rather than evicting older ones, because that many distinct
/// sources inside one interval is a flood and silence is the right answer to it.
#[derive(Debug)]
pub(crate) struct ReplyLimiter {
    interval: Duration,
    last: HashMap<IpAddr, Duration>,
}

impl ReplyLimiter {
    pub(crate) fn new(interval: Duration) -> Self {
        ReplyLimiter {
            interval,
            last: HashMap::new(),
        }
    }

    /// Whether a reply to `ip` would be allowed at `now`. Does not consume the slot, so datagrams
    /// that fail validation can be checked against it cheaply without being able to spend it.
    pub(crate) fn would_allow(&self, ip: IpAddr, now: Duration) -> bool {
        match self.last.get(&ip) {
            Some(&sent) => now.saturating_sub(sent) >= self.interval,
            None => self.last.len() < MAX_TRACKED_IPS || self.has_expired(now),
        }
    }

    /// Spend the slot for `ip` at `now`. `false` means the reply must not be sent.
    pub(crate) fn try_acquire(&mut self, ip: IpAddr, now: Duration) -> bool {
        if let Some(&sent) = self.last.get(&ip) {
            if now.saturating_sub(sent) < self.interval {
                return false;
            }
        } else if self.last.len() >= MAX_TRACKED_IPS {
            let interval = self.interval;
            self.last
                .retain(|_, &mut sent| now.saturating_sub(sent) < interval);
            if self.last.len() >= MAX_TRACKED_IPS {
                return false;
            }
        }
        self.last.insert(ip, now);
        true
    }

    fn has_expired(&self, now: Duration) -> bool {
        self.last
            .values()
            .any(|&sent| now.saturating_sub(sent) >= self.interval)
    }
}

#[derive(Debug, Default)]
struct GateInner {
    total: usize,
    per_ip: HashMap<IpAddr, usize>,
}

/// Counts unauthenticated connections, in total and per source IP.
#[derive(Debug)]
pub(crate) struct ConnGate {
    inner: Mutex<GateInner>,
    max_total: usize,
    max_per_ip: usize,
}

impl ConnGate {
    pub(crate) fn new(max_total: usize, max_per_ip: usize) -> Arc<Self> {
        Arc::new(ConnGate {
            inner: Mutex::new(GateInner::default()),
            max_total,
            max_per_ip,
        })
    }

    /// Admit a new connection from `ip`, or `None` when a cap is reached.
    pub(crate) fn try_admit(self: &Arc<Self>, ip: IpAddr) -> Option<AdmitGuard> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let from_ip = inner.per_ip.get(&ip).copied().unwrap_or(0);
        if inner.total >= self.max_total || from_ip >= self.max_per_ip {
            return None;
        }
        inner.total += 1;
        inner.per_ip.insert(ip, from_ip + 1);
        Some(AdmitGuard {
            gate: Arc::clone(self),
            ip,
        })
    }

    /// Connections currently counted.
    pub(crate) fn unauthenticated(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).total
    }
}

/// Holds one slot of the [`ConnGate`] until dropped (or until the connection authenticates).
#[derive(Debug)]
pub(crate) struct AdmitGuard {
    gate: Arc<ConnGate>,
    ip: IpAddr,
}

impl Drop for AdmitGuard {
    fn drop(&mut self) {
        let mut inner = self.gate.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.total = inner.total.saturating_sub(1);
        if let Some(count) = inner.per_ip.get_mut(&self.ip) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                inner.per_ip.remove(&self.ip);
            }
        }
    }
}

/// Allows a limited number of log lines per event code per minute, so a flood of bad handshakes
/// cannot flood the log. Counts what it suppressed.
#[derive(Debug)]
pub(crate) struct LogGate {
    per_code_per_minute: u32,
    state: Mutex<HashMap<&'static str, (Duration, u32)>>,
}

impl LogGate {
    pub(crate) fn new(per_code_per_minute: u32) -> Self {
        LogGate {
            per_code_per_minute,
            state: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn allow(&self, code: &'static str, now: Duration) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let entry = state.entry(code).or_insert((now, 0));
        if now.saturating_sub(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0);
        }
        if entry.1 < self.per_code_per_minute {
            entry.1 += 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    #[test]
    fn per_ip_bucket_allows_ten_then_limits_then_refills() {
        let mut limiter = HandshakeLimiter::new(10, 60, Duration::ZERO);
        for _ in 0..10 {
            assert!(limiter.admit(ip(1), Duration::ZERO));
        }
        assert!(!limiter.admit(ip(1), Duration::ZERO));
        // 10 per minute is one token every 6 seconds.
        assert!(!limiter.admit(ip(1), Duration::from_secs(5)));
        assert!(limiter.admit(ip(1), Duration::from_secs(7)));
    }

    #[test]
    fn an_abusive_ip_does_not_drain_the_overall_bucket() {
        let mut limiter = HandshakeLimiter::new(10, 60, Duration::ZERO);
        for _ in 0..1000 {
            let _ = limiter.admit(ip(1), Duration::ZERO);
        }
        // 10 were spent by the abuser; 50 remain for others.
        let mut others = 0;
        for i in 0..200u8 {
            if limiter.admit(ip(10 + i % 100), Duration::ZERO) {
                others += 1;
            }
        }
        assert_eq!(others, 50);
    }

    #[test]
    fn overall_bucket_applies_across_ips() {
        let mut limiter = HandshakeLimiter::new(10, 60, Duration::ZERO);
        let mut admitted = 0;
        for i in 0..100u8 {
            if limiter.admit(ip(i), Duration::ZERO) {
                admitted += 1;
            }
        }
        assert_eq!(admitted, 60);
    }

    #[test]
    fn tracked_ips_are_bounded() {
        let mut limiter = HandshakeLimiter::new(10, 1_000_000, Duration::ZERO);
        for i in 0..5000u32 {
            let ip = IpAddr::from(i.to_be_bytes());
            limiter.admit(ip, Duration::from_millis(u64::from(i)));
        }
        assert!(limiter.per_ip.len() <= MAX_TRACKED_IPS);
    }

    #[test]
    fn replies_are_limited_to_one_per_ip_per_interval() {
        let mut limiter = ReplyLimiter::new(Duration::from_secs(2));
        assert!(limiter.would_allow(ip(1), Duration::ZERO));
        assert!(limiter.try_acquire(ip(1), Duration::ZERO));
        assert!(!limiter.would_allow(ip(1), Duration::from_millis(1999)));
        assert!(!limiter.try_acquire(ip(1), Duration::from_millis(1999)));
        // Other sources are independent.
        assert!(limiter.try_acquire(ip(2), Duration::from_millis(1999)));
        // Exactly one interval later the slot is free again.
        assert!(limiter.would_allow(ip(1), Duration::from_secs(2)));
        assert!(limiter.try_acquire(ip(1), Duration::from_secs(2)));
        assert!(!limiter.try_acquire(ip(1), Duration::from_secs(3)));
    }

    #[test]
    fn checking_a_reply_slot_does_not_spend_it() {
        let limiter = ReplyLimiter::new(Duration::from_secs(2));
        for _ in 0..10 {
            assert!(limiter.would_allow(ip(1), Duration::ZERO));
        }
    }

    #[test]
    fn a_full_reply_table_refuses_new_sources_until_entries_expire() {
        let mut limiter = ReplyLimiter::new(Duration::from_secs(2));
        for i in 0..MAX_TRACKED_IPS as u32 {
            assert!(limiter.try_acquire(IpAddr::from(i.to_be_bytes()), Duration::ZERO));
        }
        let newcomer = IpAddr::from([9, 9, 9, 9]);
        assert!(!limiter.would_allow(newcomer, Duration::from_secs(1)));
        assert!(!limiter.try_acquire(newcomer, Duration::from_secs(1)));
        assert_eq!(limiter.last.len(), MAX_TRACKED_IPS);
        // Once the old entries are past their interval the newcomer fits and the table shrinks.
        assert!(limiter.would_allow(newcomer, Duration::from_secs(2)));
        assert!(limiter.try_acquire(newcomer, Duration::from_secs(2)));
        assert_eq!(limiter.last.len(), 1);
    }

    #[test]
    fn connection_gate_enforces_both_caps_and_releases() {
        let gate = ConnGate::new(32, 4);
        let held: Vec<_> = (0..4).map(|_| gate.try_admit(ip(1)).unwrap()).collect();
        assert!(gate.try_admit(ip(1)).is_none());
        assert!(gate.try_admit(ip(2)).is_some());
        drop(held);
        assert!(gate.try_admit(ip(1)).is_some());

        let gate = ConnGate::new(32, 4);
        let mut held = Vec::new();
        for i in 0..32 {
            held.push(gate.try_admit(ip(i)).unwrap());
        }
        assert!(gate.try_admit(ip(200)).is_none());
        assert_eq!(gate.unauthenticated(), 32);
        held.pop();
        assert!(gate.try_admit(ip(200)).is_some());
    }

    #[test]
    fn log_gate_caps_lines_per_code() {
        let gate = LogGate::new(3);
        let allowed = (0..10).filter(|_| gate.allow("x", Duration::ZERO)).count();
        assert_eq!(allowed, 3);
        assert!(gate.allow("y", Duration::ZERO));
        assert!(gate.allow("x", Duration::from_secs(61)));
    }
}
