use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
    fn monotonic_ms(&self) -> u64;
}

#[derive(Clone)]
pub struct SystemClock {
    origin: Arc<Instant>,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: Arc::new(Instant::now()),
        }
    }
}

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn monotonic_ms(&self) -> u64 {
        self.origin.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
}

#[derive(Clone)]
pub struct ManualClock {
    inner: Arc<std::sync::Mutex<(DateTime<Utc>, u64)>>,
}

impl ManualClock {
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            inner: Arc::new(std::sync::Mutex::new((now, 0))),
        }
    }

    pub fn advance(&self, duration: Duration) {
        let mut value = self.inner.lock().expect("manual clock lock poisoned");
        value.0 += chrono::Duration::from_std(duration).expect("test duration is valid");
        value.1 = value
            .1
            .saturating_add(duration.as_millis().min(u64::MAX as u128) as u64);
    }
}

impl Clock for ManualClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.inner.lock().expect("manual clock lock poisoned").0
    }

    fn monotonic_ms(&self) -> u64 {
        self.inner.lock().expect("manual clock lock poisoned").1
    }
}
