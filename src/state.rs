use crate::memory::Secret;
use std::time::Duration;

pub struct State {
    pending: Option<(Secret, Duration)>,
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
    pub fn install(&mut self, secret: Secret, now: Duration, ttl: Duration) {
        self.pending = Some((secret, now + ttl));
    }
    pub fn expire(&mut self, now: Duration) {
        if self
            .pending
            .as_ref()
            .is_some_and(|(_, until)| now >= *until)
        {
            self.clear();
        }
    }
    pub fn take(&mut self, now: Duration) -> Option<Secret> {
        self.expire(now);
        self.pending.take().map(|(s, _)| s)
    }
    pub fn pending(&self) -> Option<&Secret> {
        self.pending.as_ref().map(|(s, _)| s)
    }
    pub fn clear(&mut self) {
        self.pending = None;
    }
    pub fn remaining(&mut self, now: Duration) -> Option<Duration> {
        self.expire(now);
        self.pending
            .as_ref()
            .map(|(_, until)| until.saturating_sub(now))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn value(b: &[u8]) -> Secret {
        let mut s = Secret::new(64).unwrap();
        s.append(b).unwrap();
        s
    }
    #[test]
    fn expiry_replacement_and_one_shot() {
        let mut s = State::new();
        let t = Duration::from_secs(100);
        s.install(value(b"old"), t, Duration::from_secs(45));
        s.install(value(b"new"), t, Duration::from_secs(45));
        assert_eq!(s.take(t).unwrap().bytes(), b"new");
        assert!(s.take(t).is_none());
        s.install(value(b"expired"), t, Duration::from_secs(45));
        assert!(s.take(t + Duration::from_secs(45)).is_none());
        s.install(value(b"clear"), t, Duration::from_secs(45));
        s.clear();
        assert!(s.take(t).is_none());
    }
}
