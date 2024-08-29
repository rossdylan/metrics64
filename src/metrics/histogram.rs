use std::{
    any::Any,
    sync::{atomic, Arc, Mutex},
};

/// Our histogram will be the log-exponential based histogram defined by otel.
/// Seems tdigest has fallen to the wayside, and the general consensus?? is
/// log-exponential histograms are the thing. I think ddsketch, otel,
/// and prometheus all implement some version of it. I think the implementation
/// should be fairly straight forward, but we'll see. The go for it seems
/// reasonable, at least compared to the weirdness of tdigest
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
