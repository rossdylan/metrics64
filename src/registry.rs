use gxhash::{HashMap, HashSet};
use opentelemetry_proto::tonic::{
    collector::metrics::v1::{
        metrics_service_client::MetricsServiceClient, ExportMetricsServiceRequest,
    },
    common::v1::{any_value::Value, AnyValue, KeyValue},
    metrics::v1::{
        metric::Data as OTelMetricData, number_data_point, AggregationTemporality, Gauge,
        Metric as OTelMetric, NumberDataPoint, ResourceMetrics, ScopeMetrics, Sum,
    },
};
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;
use std::{
    collections::hash_map::Entry,
    hash::{Hash, Hasher},
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tonic::transport::{Channel, Endpoint};

use crate::metrics::{Metric, MetricValue, Recordable, Target};
const DEFAULT_COLLECTOR_ADDR: &str = "http://localhost:4317";
pub static DEFAULT_REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);
const MID_SEED: i64 = 0xdeadbeef;
const NANOS_PER_SEC: u64 = 1_000_000_000;

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
    /// track them in a [`HashSet`]. Ideally we'd arena allocate the actual strings
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

#[derive(Copy, Clone)]
pub struct StartTs {
    instant: Instant,
    unix_ns: u64,
}

/// This might be a turbo-over-optimization, but w/e. OTel wants both a start timestamp and a current timestamp
/// for each sample. In order to make this easy, and avoid any problems with clock skew we record the current
/// unix time once, and then use the monotonic clock provided by [`Instant`] to add seconds to our base
/// time stamp whenever we need it.
impl StartTs {
    pub fn new() -> Self {
        Self {
            instant: Instant::now(),
            unix_ns: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("duration since epoch must not be invalid")
                .as_secs()
                * NANOS_PER_SEC,
        }
    }

    /// Return our starting timestamp, and the current timestamp as nanoseconds since the
    /// unix epoch. We only support second resolution, so we take the seconds and convert into
    /// nanos instead of using a full nanosecond resolution.
    pub fn now(&self) -> (u64, u64) {
        (
            self.unix_ns,
            self.unix_ns + (self.instant.elapsed().as_secs() * NANOS_PER_SEC),
        )
    }
}

pub struct Collector {
    registry: &'static Registry,
    client: MetricsServiceClient<Channel>,
    start_time_ts: Mutex<Option<StartTs>>,
    metrics_gauge: crate::Gauge,
}

impl Collector {
    async fn collect(&mut self) {
        let (start_ts, collection_ts) = self
            .start_time_ts
            .lock()
            .get_or_insert_with(StartTs::new)
            .now();
        let collection_start = Instant::now();
        let otel_metrics = {
            let metrics = self.registry.metrics.read();
            let mut otel_metrics = Vec::with_capacity(metrics.len());
            self.metrics_gauge.set(metrics.len() as i64);
            for metric in metrics.values() {
                let value = metric.metric.value();
                let attributes: Vec<KeyValue> = metric
                    .tags
                    .iter()
                    .map(|(k, v)| KeyValue {
                        key: k.to_string(),
                        value: Some(AnyValue {
                            value: Some(Value::StringValue(v.to_string())),
                        }),
                    })
                    .collect();
                let data = match value {
                    MetricValue::Counter(v) => OTelMetricData::Sum(Sum {
                        data_points: vec![NumberDataPoint {
                            attributes,
                            start_time_unix_nano: start_ts,
                            time_unix_nano: collection_ts,
                            exemplars: Vec::new(),
                            flags: 0,
                            value: Some(number_data_point::Value::AsInt(v as i64)),
                        }],
                        aggregation_temporality: AggregationTemporality::Delta as i32,
                        is_monotonic: false,
                    }),
                    MetricValue::Gauge(v) => OTelMetricData::Gauge(Gauge {
                        data_points: vec![NumberDataPoint {
                            attributes,
                            start_time_unix_nano: 0,
                            time_unix_nano: collection_ts,
                            exemplars: Vec::new(),
                            flags: 0,
                            value: Some(number_data_point::Value::AsInt(v)),
                        }],
                    }),
                    MetricValue::Histogram => todo!(),
                };
                otel_metrics.push(OTelMetric {
                    name: metric.name.into(),
                    description: "".into(),
                    unit: "".into(),
                    metadata: Vec::new(),
                    data: Some(data),
                });
            }
            otel_metrics
        };
        // TODO(rossdylan): Report this as a histogram, and use it to understand if we need to parallelise
        // the collection loop using rayon or something
        let collection_dur = collection_start.elapsed();
        tracing::debug!(message="collected metrics", collection_ts=collection_ts, start_ts=start_ts, duration=?collection_dur, metrics=otel_metrics.len());

        let export_res = self
            .client
            .export(ExportMetricsServiceRequest {
                resource_metrics: vec![ResourceMetrics {
                    // TODO(rossdylan): This should be common tags for this process
                    resource: None,
                    scope_metrics: vec![ScopeMetrics {
                        scope: None,
                        metrics: otel_metrics,
                        schema_url: opentelemetry_semantic_conventions::SCHEMA_URL.into(),
                    }],
                    schema_url: opentelemetry_semantic_conventions::SCHEMA_URL.into(),
                }],
            })
            .await;
        if let Err(e) = export_res {
            tracing::error!(message="failed to export metrics", status=?e);
        }
    }
}

impl Registry {
    pub fn new() -> Self {
        Self {
            metrics: Default::default(),
            interner: Default::default(),
        }
    }

    pub fn start(&'static self) {
        self.start_with_url(DEFAULT_COLLECTOR_ADDR.into());
    }

    pub fn start_with_url(&'static self, collector_url: String) {
        let channel = Endpoint::from_shared(collector_url)
            .expect("collector endpoint must be valid")
            .connect_lazy();
        let collector = Collector {
            registry: self,
            client: MetricsServiceClient::new(channel),
            start_time_ts: Default::default(),
            metrics_gauge: metrics::REGISTERED_METRICS.must(&[]),
        };
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

pub(crate) mod metrics {
    use crate::GaugeDef;

    pub const REGISTERED_METRICS: GaugeDef =
        GaugeDef::new("metrics64/registry/metrics", crate::Target::Pod, &[]);
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
    }
}
