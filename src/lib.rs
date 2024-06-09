use gxhash::HashMap;
use parking_lot::RwLock;
use std::any::Any;
use std::collections::hash_map::Entry;
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
#[derive(Debug, Copy, Clone)]
pub enum Target {
    /// Emit per-pod related location information
    Pod,
}

/// A trait representing the public metric interface. This is common across all metric kinds, and slots into the
/// general [`MetricDefinition`] to provide the backing implementation.
pub trait Metric: Sized {
    fn must(mid: u64) -> Self;
}

/// A trait representing the internal chunk of the metric interface. We use this to collect observations of the
/// underlying metric value, as well as store references to the metric in the registry.
pub trait Recordable: Send + Sync + 'static {
    fn as_any(&self) -> &dyn Any;
}

/// A constant definition of a metric. Provides a single spot for defining the schema of a metric at compile time
/// Internally it will handle calling out to the registry at runtime to register the new metric.
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

    /// A note about the ordering here.
    /// I think we can push most of the underyling metric storage/allocation down into the registry. So instead
    /// of the current back and forth with mid, get, register we just call `create` or something and internally
    /// the registry sets up our singular storage location for metadata and the metric itself and just returns
    /// the core metric. This should allow us to avoid having to spread the allocations for metric metadata
    /// into each of the individual metrics and instead we can keep them all together in the hashmap. or
    /// maybe we use a slab/pool since its all pretty small.
    pub fn must(&self, tags: &[(&'static str, &'static str)]) -> M {
        DEFAULT_REGISTRY.register(self.name, self.target, tags)
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
    fn must(_mid: u64) -> Self {
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

struct MetricMetadata {
    name: &'static str,
    target: Target,
    tags: Vec<(&'static str, &'static str)>,
    metric: Box<dyn Recordable>,
}

impl MetricMetadata {
    pub fn new<R: Recordable>(
        name: &'static str,
        target: Target,
        tags: &[(&'static str, &'static str)],
        metric: R,
    ) -> Self {
        Self {
            name,
            target,
            tags: Vec::from(tags),
            metric: Box::new(metric),
        }
    }
}

#[derive(Default)]
pub struct Registry {
    metrics: RwLock<HashMap<u64, MetricMetadata>>,
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
    fn mid(&self, name: &'static str, tags: &[(&'static str, &'static str)]) -> u64 {
        let mut hasher = gxhash::GxHasher::with_seed(MID_SEED);
        name.hash(&mut hasher);
        tags.hash(&mut hasher);
        hasher.finish()
    }

    pub fn register<R: Recordable + Metric + Clone>(
        &self,
        name: &'static str,
        target: Target,
        tags: &[(&'static str, &'static str)],
    ) -> R {
        let mid = self.mid(name, tags);
        let mut metrics = self.metrics.write();
        let entry = metrics.entry(mid);
        match entry {
            Entry::Occupied(dr) => dr
                .get()
                .metric
                .as_any()
                .downcast_ref::<R>()
                .cloned()
                .expect("attempted to register metric with same mid with different type"),
            Entry::Vacant(ve) => {
                let metric = R::must(mid);
                ve.insert(MetricMetadata::new(name, target, tags, metric.clone()));
                metric
            }
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
