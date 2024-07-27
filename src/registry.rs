use gxhash::{HashMap, HashSet};
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;
use std::{
    collections::hash_map::Entry,
    hash::{Hash, Hasher},
    sync::LazyLock,
};

use crate::metrics::{Metric, Recordable, Target};

pub static DEFAULT_REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);
const MID_SEED: i64 = 0xdeadbeef;

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
struct Interner {
    inner: Mutex<HashSet<&'static str>>,
}

impl Interner {
    /// Our string interning routine is pretty shit, we just leak heap allocations and
    /// track them in a [`HashSet`]. Ideally we'd area allocate the actual strings
    /// instead of littering them across the heap.
    fn intern_tags(&self, tags: &[(&str, &str)]) -> SmallVec<[(&'static str, &'static str); 8]> {
        let mut inner = self.inner.lock();
        let intern = |i: &mut HashSet<&'static str>, s| -> &'static str {
            if let Some(is) = i.get(s) {
                is
            } else {
                let s = String::from(s);
                let leaked: &'static str = Box::leak(s.into_boxed_str());
                i.insert(leaked);
                leaked
            }
        };
        tags.iter()
            .map(|(k, v)| (intern(&mut inner, *k), intern(&mut inner, *v)))
            .collect()
    }
}

#[derive(Default)]
pub struct Registry {
    metrics: RwLock<HashMap<u64, MetricMetadata>>,
    interner: Interner,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            metrics: Default::default(),
            interner: Default::default(),
        }
    }

    /// Calculate a metric-id for this metric. This is used as a key internally to the registry to handle lookups of
    /// registered metrics.
    /// NOTE(rossdylan): tags **must** be sorted to get an accurate mid
    fn mid(&self, name: &str, tags: &[(&str, &str)]) -> u64 {
        debug_assert!(tags.is_sorted());
        let mut hasher = gxhash::GxHasher::with_seed(MID_SEED);
        name.hash(&mut hasher);
        tags.hash(&mut hasher);
        hasher.finish()
    }

    /// This will eventually be called every N seconds to gather a snapshot of
    /// all metrics within the registry.
    fn collect(&self) {
        let metrics = self.metrics.read();

        for metric in metrics.values() {
            println!("{}{:?}", metric.name, metric.tags);
        }
    }
    /// Register is a fairly heavy weight operation. We expect that concrete metrics are cached at a higher
    /// level and statically dispatched in an app-specific way. This usage expectation is what allows us to
    /// provide a more ergonomic API. We pay the majority of the cost at register() time and then everything
    /// after is coordination free.
    pub fn register<R: Recordable + Metric + Clone>(
        &self,
        name: &'static str,
        target: Target,
        tags: &[(&str, &str)],
    ) -> R {
        // NOTE(rossdylan): We are trying to be clever here by interning, de-duping
        // and stack-allocating our tags all in one go. For small sets of tags that
        // we've seen before this avoids allocating entirely. However we do pay
        // a general penality for all the lookups and locking involved.
        let mut tags = self.interner.intern_tags(tags);

        // To ensure consistent MID generation we sort and dedupe our tags.
        // TODO(rossdylan): If we can sort and dedupe in the intern phase we
        // could potentially reduce the allocations required when pathalogical
        // duplicated tags are input.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CounterDef;
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
        println!("{:#?}", DEFAULT_REGISTRY.metrics.read().keys());
        // NOTE(rossdylan): This reg is global so this includes the other test metrics
        // as well.
        assert_eq!(DEFAULT_REGISTRY.metrics.read().len(), 2);
    }
}
