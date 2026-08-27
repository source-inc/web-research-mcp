use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

#[derive(Debug)]
pub struct Session {
    pub session_id: String,
    pub created_at: Instant,
    pub last_used_at: Instant,
    pub ttl: Duration,
    pub idle: Duration,
}

#[derive(Clone)]
pub struct SessionRegistry {
    inner: Arc<Mutex<HashMap<String, Session>>>,
    max_concurrent: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AllocError {
    AtCapacity,
}

#[derive(Debug, PartialEq, Eq)]
pub enum LookupError {
    NotFound,
    Expired,
}

impl SessionRegistry {
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
        }
    }

    pub fn allocate(
        &self,
        session_id: String,
        ttl: Duration,
        idle: Duration,
    ) -> Result<(), AllocError> {
        let mut map = self.inner.lock();
        self.gc_locked(&mut map);
        if map.len() >= self.max_concurrent {
            return Err(AllocError::AtCapacity);
        }
        let now = Instant::now();
        map.insert(
            session_id.clone(),
            Session {
                session_id,
                created_at: now,
                last_used_at: now,
                ttl,
                idle,
            },
        );
        Ok(())
    }

    pub fn touch(&self, session_id: &str) -> Result<(), LookupError> {
        let mut map = self.inner.lock();
        let session = map.get_mut(session_id).ok_or(LookupError::NotFound)?;
        let now = Instant::now();
        if now.duration_since(session.created_at) >= session.ttl
            || now.duration_since(session.last_used_at) >= session.idle
        {
            map.remove(session_id);
            return Err(LookupError::Expired);
        }
        session.last_used_at = now;
        Ok(())
    }

    pub fn remove(&self, session_id: &str) -> bool {
        let mut map = self.inner.lock();
        map.remove(session_id).is_some()
    }

    pub fn count(&self) -> usize {
        self.inner.lock().len()
    }

    /// Evict all expired sessions; returns evicted IDs.
    pub fn sweep(&self) -> Vec<String> {
        let mut map = self.inner.lock();
        self.gc_locked(&mut map)
    }

    fn gc_locked(&self, map: &mut HashMap<String, Session>) -> Vec<String> {
        let now = Instant::now();
        let expired: Vec<String> = map
            .iter()
            .filter(|(_, s)| {
                now.duration_since(s.created_at) >= s.ttl
                    || now.duration_since(s.last_used_at) >= s.idle
            })
            .map(|(k, _)| k.clone())
            .collect();
        for id in &expired {
            map.remove(id);
        }
        expired
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn allocate_under_cap() {
        let r = SessionRegistry::new(2);
        assert!(r
            .allocate("a".into(), Duration::from_secs(60), Duration::from_secs(60))
            .is_ok());
        assert!(r
            .allocate("b".into(), Duration::from_secs(60), Duration::from_secs(60))
            .is_ok());
    }

    #[test]
    fn allocate_at_cap_fails() {
        let r = SessionRegistry::new(1);
        r.allocate("a".into(), Duration::from_secs(60), Duration::from_secs(60))
            .unwrap();
        assert_eq!(
            r.allocate("b".into(), Duration::from_secs(60), Duration::from_secs(60)),
            Err(AllocError::AtCapacity)
        );
    }

    #[test]
    fn touch_unknown_session() {
        let r = SessionRegistry::new(1);
        assert_eq!(r.touch("nope"), Err(LookupError::NotFound));
    }

    #[test]
    fn ttl_expiration() {
        let r = SessionRegistry::new(1);
        r.allocate(
            "a".into(),
            Duration::from_millis(20),
            Duration::from_secs(60),
        )
        .unwrap();
        sleep(Duration::from_millis(40));
        assert_eq!(r.touch("a"), Err(LookupError::Expired));
        assert_eq!(r.count(), 0);
    }

    #[test]
    fn idle_expiration() {
        let r = SessionRegistry::new(1);
        r.allocate(
            "a".into(),
            Duration::from_secs(60),
            Duration::from_millis(20),
        )
        .unwrap();
        sleep(Duration::from_millis(40));
        assert_eq!(r.touch("a"), Err(LookupError::Expired));
    }

    #[test]
    fn remove_session() {
        let r = SessionRegistry::new(2);
        r.allocate("a".into(), Duration::from_secs(60), Duration::from_secs(60))
            .unwrap();
        assert!(r.remove("a"));
        assert!(!r.remove("a"));
    }

    #[test]
    fn cap_recovers_after_sweep() {
        let r = SessionRegistry::new(1);
        r.allocate(
            "a".into(),
            Duration::from_millis(10),
            Duration::from_secs(60),
        )
        .unwrap();
        sleep(Duration::from_millis(30));
        // Expired but still in the map until next op.
        assert!(r
            .allocate("b".into(), Duration::from_secs(60), Duration::from_secs(60))
            .is_ok());
    }
}
