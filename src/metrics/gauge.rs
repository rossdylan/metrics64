use std::{
    any::Any,
    sync::{atomic, Arc},
};

#[derive(Clone)]
pub struct Gauge {
    inner: Arc<atomic::AtomicI64>,
}

impl Gauge {
    pub fn incr_by(&self, count: i64) {
        self.inner.fetch_add(count, atomic::Ordering::Relaxed);
    }

    pub fn incr(&self) {
        self.incr_by(1);
    }

    pub fn set(&self, val: i64) {
        self.inner.store(val, atomic::Ordering::Relaxed)
    }

    pub fn decr(&self) {
        self.incr_by(-1)
    }
}

impl super::Metric for Gauge {
    fn must(_mid: u64) -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl super::Recordable for Gauge {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
