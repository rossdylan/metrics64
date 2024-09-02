use std::{
    any::Any,
    sync::{atomic, Arc},
};

use super::MetricValue;

/// A simple delta counter used to record the rate of actions happening. The
/// value of the counter is reset when read, thus we always report the delta
/// between times.
#[derive(Clone)]
pub struct Counter {
    inner: Arc<atomic::AtomicU64>,
}

impl Counter {
    pub fn incr_by(&self, count: u64) {
        self.inner.fetch_add(count, atomic::Ordering::Relaxed);
    }

    pub fn incr(&self) {
        self.incr_by(1);
    }
}

impl super::Metric for Counter {
    fn must(_mid: u64) -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl super::Recordable for Counter {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn value(&self) -> MetricValue {
        MetricValue::Counter(self.inner.swap(0, atomic::Ordering::Relaxed))
    }
}
