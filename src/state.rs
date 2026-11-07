use crate::memory::Secret;
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};
static NEXT: AtomicU64 = AtomicU64::new(1);
struct Pending {
    secret: Secret,
    until: Duration,
    generation: u64,
    recipient: Option<String>,
}
pub struct State {
    pending: Option<Pending>,
}
impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}
impl State {
    pub fn new() -> Self {
        Self { pending: None }
    }
    pub fn install(
        &mut self,
        secret: Secret,
        recipient: Option<String>,
        now: Duration,
        ttl: Duration,
    ) {
        let generation = NEXT.fetch_add(1, Ordering::Relaxed);
        assert!(generation != u64::MAX, "generation exhausted");
        self.pending = Some(Pending {
            secret,
            until: now + ttl,
            generation,
            recipient,
        });
    }
    pub fn expire(&mut self, now: Duration) {
        if self.pending.as_ref().is_some_and(|p| now >= p.until) {
            self.clear();
        }
    }
    pub fn generation(&self) -> Option<u64> {
        self.pending.as_ref().map(|p| p.generation)
    }
    pub fn recipient(&self) -> Option<&str> {
        self.pending.as_ref().and_then(|p| p.recipient.as_deref())
    }
    pub fn take_if(&mut self, generation: u64, now: Duration) -> Option<Secret> {
        self.expire(now);
        if self.generation() != Some(generation) {
            return None;
        }
        self.pending.take().map(|p| p.secret)
    }
    pub fn pending(&self) -> Option<&Secret> {
        self.pending.as_ref().map(|p| &p.secret)
    }
    pub fn clear(&mut self) {
        self.pending = None;
    }
    pub fn remaining(&mut self, now: Duration) -> Option<Duration> {
        self.expire(now);
        self.pending.as_ref().map(|p| p.until.saturating_sub(now))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn value() -> Secret {
        let mut s = Secret::new(32).unwrap();
        s.append(b"fixture").unwrap();
        s
    }
    #[test]
    fn generations_prevent_replacement_and_recreation_races() {
        let now = Duration::from_secs(1);
        let ttl = Duration::from_secs(45);
        let mut s = State::new();
        s.install(value(), None, now, ttl);
        let old = s.generation().unwrap();
        s.install(value(), Some("recipient".into()), now, ttl);
        assert_eq!(s.recipient(), Some("recipient"));
        assert!(s.take_if(old, now).is_none());
        assert!(s.pending().is_some());
        let current = s.generation().unwrap();
        assert!(s.take_if(current, now).is_some());
        assert!(s.take_if(current, now).is_none());
        s.install(value(), None, now, ttl);
        assert!(s.recipient().is_none());
        let current = s.generation().unwrap();
        assert!(s.take_if(current, now + ttl).is_none());
        s.install(value(), None, now, ttl);
        let old = s.generation().unwrap();
        s.clear();
        let mut recreated = State::new();
        recreated.install(value(), None, now, ttl);
        assert!(recreated.take_if(old, now).is_none());
    }
}
