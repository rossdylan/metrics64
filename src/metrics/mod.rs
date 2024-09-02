use std::{any::Any, marker::PhantomData};

use crate::{registry::Registry, DEFAULT_REGISTRY};

pub(crate) mod counter;
pub(crate) mod gauge;
pub(crate) mod histogram;

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

#[derive(Copy, Clone)]
pub enum MetricValue {
    Counter(u64),
    Gauge(i64),
    Histogram,
}

/// A trait representing the internal chunk of the metric interface. We use this to collect observations of the
/// underlying metric value, as well as store references to the metric in the registry.
pub trait Recordable: Send + Sync + 'static {
    /// We define this helper method on Recordable to allow our registry implementations to
    /// de-virtualize (is this right term?) our `dyn Recordable` back into the actual
    /// concrete [`Metric`] the user expects.
    fn as_any(&self) -> &dyn Any;

    fn value(&self) -> MetricValue;
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
    pub fn must(&self, tags: &[(&str, &str)]) -> M {
        self.must_with_registry(&DEFAULT_REGISTRY, tags)
    }

    #[doc(hidden)]
    pub fn must_with_registry(&self, registry: &Registry, tags: &[(&str, &str)]) -> M {
        // TODO(rossdylan): validate incoming tags based on schema in definition

        registry.register(self.name, self.target, tags)
    }
}
