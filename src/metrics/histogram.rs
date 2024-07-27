use std::{
    any::Any,
    sync::{atomic, Arc, Mutex},
};

#[derive(Default)]
struct HistogramInner {}

/// A threadsafe histogram for tracking the distribution of values in an
/// efficient way.
#[derive(Default)]
pub struct Histogram {
    inner: Mutex<HistogramInner>,
}

impl Histogram {
    pub fn observe(&self, value: f32) {
        todo!()
    }
}

impl super::Metric for Histogram {
    fn must(_mid: u64) -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl super::Recordable for Histogram {
    fn as_any(&self) -> &dyn Any {
        self
    }
}
