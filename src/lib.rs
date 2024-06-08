use gxhash::HashMap;
use parking_lot::RwLock;
use std::any::Any;
use std::{
    hash::{Hash, Hasher},
    marker::PhantomData,
    sync::{atomic::AtomicU64, Arc, LazyLock},
};

/// The api I'm thinking about here is a cross between  Google's Monarch and Dropbox's vortex2
/// let push_counter_def = metrics64::DefineCounter(
///     "metrics64/exporter/pushes",    // Metric name
///     metrics64::targets::Pod,       // target metric (the location this metric is exported from)
///     &["status"],                // tags associated with this metric
/// );
/// let push_counter = push_counter_def.new("status", "ok");

/// An enum that defines a set of global location tags that are evalulated once per process
/// to provide location/targetting information.
pub enum Target {
    /// Emit per-pod related location information
    Pod,
}

pub trait Metric: Sized {
    fn must() -> Self;
}

pub trait Recordable: Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;
}

pub struct MetricDef<M> {
    name: &'static str,
    tags: &'static [&'static str],
    target: Target,
    _kind: PhantomData<M>,
}

impl<M> MetricDef<M>
where
    M: Metric + Recordable + Clone,
{
    pub const fn new(name: &'static str, target: Target, tags: &'static [&'static str]) -> Self {
        Self {
            name,
            tags,
            target,
            _kind: PhantomData,
        }
    }

    pub fn must(&self, tags: &[(&'static str, &'static str)]) -> M {
        let mid = DEFAULT_REGISTRY.mid(self.name, tags);
        if let Some(m) = DEFAULT_REGISTRY.get(mid) {
            return m;
        }
        let m = M::must();
        DEFAULT_REGISTRY.register(mid, m.clone());
        m
    }
}

#[derive(Clone)]
pub struct Counter {
    inner: Arc<AtomicU64>,
}

impl Counter {
    pub fn incr_by(&self, count: u64) {
        self.inner
            .fetch_add(count, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn incr(&self) {
        self.incr_by(1);
    }
}

impl Metric for Counter {
    fn must() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl Recordable for Counter {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Default)]
pub struct Registry {
    metrics: RwLock<HashMap<u64, Box<dyn Recordable>>>,
}

static DEFAULT_REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);
const MID_SEED: i64 = 0xdeadbeef;
impl Registry {
    pub fn new() -> Self {
        Self {
            metrics: Default::default(),
        }
    }

    /// Calculate a metric-id for this metric. This is used as a key internally to the registry to handle lookups of
    /// registered metrics.
    pub fn mid(&self, name: &'static str, tags: &[(&'static str, &'static str)]) -> u64 {
        let mut hasher = gxhash::GxHasher::with_seed(MID_SEED);
        name.hash(&mut hasher);
        tags.hash(&mut hasher);
        hasher.finish()
    }

    /// Attempt to get the metric registered under the givee mid. We do some downcasting magic to return
    /// the underlying metric if it exists. This ensures that if somehow we attempt to create the same
    /// exact metric twice they will share the underlying metric implementation.
    pub fn get<R: Recordable + Clone>(&self, mid: u64) -> Option<R> {
        let metrics = self.metrics.read();
        metrics
            .get(&mid)
            .and_then(|m| m.as_any().downcast_ref::<R>())
            .cloned()
    }

    pub fn register<R: Recordable>(&self, mid: u64, metric: R) {
        if self.metrics.write().insert(mid, Box::new(metric)).is_some() {
            panic!("attempted to double registry metric {mid}");
        }
    }
}

pub type CounterDef = MetricDef<Counter>;

#[cfg(test)]
mod tests {
    use super::*;
    const ALLEYCAT_CLIENT: CounterDef = CounterDef::new(
        "alleycat/client/requests",
        Target::Pod,
        &["service", "method", "status"],
    );

    #[test]
    fn test_counter_must() {
        let counter = ALLEYCAT_CLIENT.must(&[
            ("service", "metrics64.MetricCollector"),
            ("method", "Push"),
            ("status", "ok"),
        ]);

        let counter2_same = ALLEYCAT_CLIENT.must(&[
            ("service", "metrics64.MetricCollector"),
            ("method", "Push"),
            ("status", "ok"),
        ]);
        counter.incr();
        counter2_same.incr();
        assert_eq!(counter.inner.load(std::sync::atomic::Ordering::Relaxed), 2);
        println!("{:#?}", DEFAULT_REGISTRY.metrics.read().keys());
        assert_eq!(DEFAULT_REGISTRY.metrics.read().len(), 1);
    }
}
