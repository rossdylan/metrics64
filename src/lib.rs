//! The api I'm thinking about here is a cross between  Google's Monarch and Dropbox's vortex2
//! let push_counter_def = metrics64::DefineCounter(
//!     "metrics64/exporter/pushes",    // Metric name
//!     metrics64::targets::Pod,       // target metric (the location this metric is exported from)
//!     &["status"],                // tags associated with this metric
//! );
//! let push_counter = push_counter_def.new("status", "ok");
//! There are a few core tenants I want to focus on for metrics64
//! 1. metrics64 does almost all the work, string internment, registries, exports, etc are all metrics64 job
//! 2. dispatch from some app condition to a specific metric op is the applications responsibility
//! 3. no crazy macro magic. Try and leverage the type system and runtime amortization to deal with the complexity

use gxhash::HashMap;
use parking_lot::RwLock;
use smallvec::SmallVec;
use std::any::Any;
use std::collections::hash_map::Entry;
use std::collections::BTreeSet;
use std::sync::atomic::AtomicI64;
use std::{
    hash::{Hash, Hasher},
    marker::PhantomData,
    sync::{atomic::AtomicU64, Arc, LazyLock},
};

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
pub struct Gauge {
    inner: Arc<AtomicI64>,
}

impl Gauge {
    pub fn incr_by(&self, count: i64) {
        self.inner
            .fetch_add(count, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn incr(&self) {
        self.incr_by(1);
    }

    pub fn set(&self, val: i64) {
        self.inner.store(val, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn decr(&self) {
        self.incr_by(-1)
    }
}

impl Metric for Gauge {
    fn must(_mid: u64) -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl Recordable for Gauge {
    fn as_any(&self) -> &dyn Any {
        self
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
    tags: SmallVec<[(&'static str, &'static str); 8]>,
    metric: Box<dyn Recordable>,
}

impl MetricMetadata {
    pub fn new<R: Recordable>(
        name: &'static str,
        target: Target,
        tags: SmallVec<[(&'static str, &'static str); 8]>,
        metric: R,
    ) -> Self {
        Self {
            name,
            target,
            tags,
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
    /// NOTE(rossdylan): tags **must** be sorted to get an accurate mid
    fn mid(&self, name: &'static str, tags: &[(&'static str, &'static str)]) -> u64 {
        let mut hasher = gxhash::GxHasher::with_seed(MID_SEED);
        name.hash(&mut hasher);
        tags.hash(&mut hasher);
        hasher.finish()
    }

    /// Register is a fairly heavy weight operation. We expect that concrete metrics are cached at a higher
    /// level and statically dispatched in an app-specific way. This usage expectation is what allows us to
    /// provide a more ergonomic API. We pay the majority of the cost at register() time and then everything
    /// after is coordination free.
    pub fn register<R: Recordable + Metric + Clone>(
        &self,
        name: &'static str,
        target: Target,
        tags: &[(&'static str, &'static str)],
    ) -> R {
        // TODO(rossdylan): Weaking lifetime reqs on the tags and intern them instead
        // NOTE(rossdylan): We are potentially being overly clever here. Instead
        // of copying the whole thing into a BTreeSet like we had originally we
        // instead use a SmallVec that can hold 8 tags inline on the stack. The
        // idea is that we have cascading levels of "fast" paths
        // 1. <= 8 tags, existing => no allocs, 1 lock, 1 hashmap lookup
        // 2. <= 8 tags, new => 2 allocs, 1 lock, 1 hashmap insert
        // 3. > 8 tags, existing => 1 alloc, 1 lock, 1 hashmap lookup
        // 3. > 8 tags, new => 3 alloc, 1 lock, 1 hashmap insert
        let mut tags: SmallVec<[(&'static str, &'static str); 8]> = SmallVec::from_slice(tags);
        tags.sort_unstable();
        tags.dedup();
        let mid = self.mid(name, &tags);
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
        counter.incr()
    }

    #[test]
    fn test_counter_dispatch() {
        let counter = ALLEYCAT_CLIENT.must(&[
            ("service", "metrics64.MetricCollector"),
            ("method", "Push"),
            ("status", "test"),
        ]);

        let counter2 = ALLEYCAT_CLIENT.must(&[
            ("service", "metrics64.MetricCollector"),
            ("status", "test"),
            ("method", "Push"),
        ]);

        let counter3 = counter2.clone();
        counter.incr();
        counter2.incr();
        counter3.incr();
        assert_eq!(counter.inner.load(std::sync::atomic::Ordering::Relaxed), 3);
        println!("{:#?}", DEFAULT_REGISTRY.metrics.read().keys());
        // NOTE(rossdylan): This reg is global so this includes the other test metrics
        // as well.
        assert_eq!(DEFAULT_REGISTRY.metrics.read().len(), 2);
    }
}
