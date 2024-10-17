use std::{any::Any, marker::PhantomData};

use crate::{registry::Registry, DEFAULT_REGISTRY};

pub(crate) mod counter;
pub(crate) mod gauge;
pub(crate) mod histogram;

/// A trait representing the public metric interface. This is common across all metric kinds, and slots into the
/// general [`MetricDefinition`] to provide the backing implementation.
pub trait Metric: Sized {
    fn must() -> Self;
}

#[derive(Clone)]
pub enum MetricValue {
    Counter(u64),
    Gauge(i64),
    Histogram {
        scale: i32,
        count: u64,
        zero_count: u64,
        sum: f64,
        min: f64,
        max: f64,
        positive: (i32, Vec<u64>),
        negative: (i32, Vec<u64>),
    },
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

pub trait Tags<'a> {
    fn tags(&self) -> &'a [(&'a str, &'a str)];
}

impl<'a> Tags<'a> for &'a [(&'a str, &'a str)] {
    fn tags(&self) -> &'a [(&'a str, &'a str)] {
        self
    }
}

pub trait TagSchema {
    type Input<'a>: Tags<'a>;

    fn tag_keys(&self) -> &'static [&'static str];

    fn validate(&self, tags: &Self::Input<'_>);
}

impl TagSchema for &'static [&'static str] {
    type Input<'a> = &'a [(&'a str, &'a str)];
    fn tag_keys(&self) -> &'static [&'static str] {
        self
    }

    fn validate(&self, tags: &Self::Input<'_>) {
        for (key, _value) in tags.tags() {
            if !self.contains(key) {
                // TODO(rossdylan): Don't panic, and instead return an error
                panic!("passed invalid tag {key}")
            }
        }
    }
}

/// A constant definition of a metric. Provides a single spot for defining the schema of a metric at compile time
/// Internally it will handle calling out to the registry at runtime to register the new metric.
pub struct MetricDef<M, T = &'static [&'static str]> {
    name: &'static str,
    tags: T,
    _kind: PhantomData<M>,
}

impl<M, T> MetricDef<M, T>
where
    M: Metric + Recordable + Clone,
    T: TagSchema,
{
    pub const fn new(name: &'static str, tags: T) -> Self {
        Self {
            name,
            tags,
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
    pub fn must(&self, tags: T::Input<'_>) -> M {
        self.must_with_registry(&DEFAULT_REGISTRY, tags)
    }

    #[doc(hidden)]
    pub fn must_with_registry(&self, registry: &Registry, tags: T::Input<'_>) -> M {
        self.tags.validate(&tags);
        registry.register(self.name, tags.tags())
    }
}
