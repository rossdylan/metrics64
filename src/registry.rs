use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    hash::{Hash, Hasher},
    sync::{Arc, LazyLock, OnceLock},
    time::Duration,
};

use opentelemetry_proto::tonic::collector::metrics::v1::metrics_service_client::MetricsServiceClient;
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;
use tonic::transport::Endpoint;

use crate::{
    metrics::{Metric, Recordable},
    utils::BuildNoopHasher,
};

const DEFAULT_COLLECTOR_ADDR: &str = "http://localhost:4317";
pub static DEFAULT_REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);
const MID_SEED: u64 = 0xdeadbeef;

pub struct MetricMetadata {
    pub name: &'static str,
    pub tags: SmallVec<[(&'static str, &'static str); 8]>,
    pub metric: Box<dyn Recordable>,
    /// The unix timestamp of when this metric was first exported. This is used
    /// by otel to track process restarts
    pub export_start_ts: OnceLock<u64>,
}

impl MetricMetadata {
    pub fn new<R: Recordable>(
        name: &'static str,
        tags: SmallVec<[(&'static str, &'static str); 8]>,
        metric: R,
    ) -> Self {
        Self {
            name,
            tags,
            metric: Box::new(metric),
            export_start_ts: OnceLock::new(),
        }
    }
}

#[derive(Default)]
struct Interner {
    inner: Arc<Mutex<HashSet<&'static str>>>,
}

impl Interner {
    /// Our string interning routine is pretty shit, we just leak heap allocations and
    /// track them in a [`HashSet`]. Ideally we'd arena allocate the actual strings
    /// instead of littering them across the heap.
    fn intern_tags(&self, tags: &[(&str, &str)]) -> SmallVec<[(&'static str, &'static str); 8]> {
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
        let mut hs = self.inner.lock();
        tags.iter()
            .map(|(k, v)| (intern(&mut hs, *k), intern(&mut hs, *v)))
            .collect()
    }
}

#[derive(Default)]
pub struct Registry {
    pub metrics: RwLock<HashMap<u64, MetricMetadata, BuildNoopHasher>>,
    interner: Interner,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            metrics: RwLock::new(HashMap::with_hasher(BuildNoopHasher {})),
            interner: Default::default(),
        }
    }

    pub fn start(&'static self, service: &str) {
        self.start_with_url(Some(service), DEFAULT_COLLECTOR_ADDR.into());
    }

    pub fn start_with_url(&'static self, service: Option<&str>, collector_url: String) {
        let channel = Endpoint::from_shared(collector_url)
            .expect("collector endpoint must be valid")
            .connect_lazy();
        let collector =
            crate::collector::Collector::new(service, self, MetricsServiceClient::new(channel));
        // Spawn our runtime metrics collector
        tokio::spawn(crate::runtime::export_task());
        // Spawn the metrics collector which will export over otel periodically
        tokio::spawn(async move {
            let mut collector = collector;
            let mut ticker = tokio::time::interval(Duration::from_secs(10));
            loop {
                tokio::select! {
                _ = ticker.tick() => { }
                }
                collector.collect().await;
            }
        });
    }

    /// Calculate a metric-id for this metric. This is used as a key internally to the registry to handle lookups of
    /// registered metrics.
    /// NOTE(rossdylan): tags **must** be sorted to get an accurate mid
    fn mid(&self, name: &str, tags: &[(&str, &str)]) -> u64 {
        debug_assert!(tags.is_sorted());
        let mut hasher = twox_hash::XxHash64::with_seed(MID_SEED);
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
        tags: &[(&str, &str)],
    ) -> R {
        // NOTE(rossdylan): We are trying to be clever here by interning, de-duping
        // and inline-allocating our tags all in one go. For small sets of tags that
        // we've seen before this avoids allocating entirely. However we do pay
        // a general penalty for all the lookups and locking involved.
        let mut tags = self.interner.intern_tags(tags);

        // To ensure consistent MID generation we sort and dedupe our tags.
        // TODO(rossdylan): If we can sort and dedupe in the intern phase we
        // could potentially reduce the allocations required when pathological
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
                let metric = R::must();
                ve.insert(MetricMetadata::new(name, tags, metric.clone()));
                metric
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CounterDef;
    const ALLEYCAT_CLIENT: CounterDef =
        CounterDef::new("alleycat/client/requests", &["service", "method", "status"]);

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
    }
}
