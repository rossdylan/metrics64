use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    hash::{Hash, Hasher},
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use opentelemetry_proto::tonic::{
    collector::metrics::v1::{
        metrics_service_client::MetricsServiceClient, ExportMetricsServiceRequest,
    },
    common::v1::{any_value::Value, AnyValue, KeyValue},
    metrics::v1::{
        exponential_histogram_data_point::Buckets, metric::Data as OTelMetricData,
        number_data_point, AggregationTemporality, ExponentialHistogram,
        ExponentialHistogramDataPoint, Gauge, Metric as OTelMetric, NumberDataPoint,
        ResourceMetrics, ScopeMetrics, Sum,
    },
};
use parking_lot::{Mutex, RwLock};
use smallvec::SmallVec;
use tonic::transport::{Channel, Endpoint};

use crate::metrics::{Metric, MetricValue, Recordable};

const DEFAULT_COLLECTOR_ADDR: &str = "http://localhost:4317";
pub static DEFAULT_REGISTRY: LazyLock<Registry> = LazyLock::new(Registry::new);
const MID_SEED: u64 = 0xdeadbeef;
const NANOS_PER_SEC: u64 = 1_000_000_000;

struct MetricMetadata {
    name: &'static str,
    tags: SmallVec<[(&'static str, &'static str); 8]>,
    metric: Box<dyn Recordable>,
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

/// I'm not entirely sure this is worth it. Instead of using a non-monotonic
/// system time call for every export we use a base timestamp and a monotonic
/// offset. This means that regardless of clock skew that happens after process
/// start we'll always have an accurate timestamp.
/// NOTE(rossdylan): This doesn't handle the case where our clocks are skewed
/// from the start.
#[derive(Copy, Clone)]
pub struct StartTs {
    instant: Instant,
    unix_ns: u64,
}

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
    pub fn now(&self) -> u64 {
        self.unix_ns + (self.instant.elapsed().as_secs() * NANOS_PER_SEC)
    }
}

/// The collector is used to run our periodic exports to some endpoint that
/// speaks otel.
struct Collector {
    registry: &'static Registry,
    client: MetricsServiceClient<Channel>,
    start_time_ts: Option<StartTs>,
    metrics_gauge: crate::Gauge,
    export_latency_ms: crate::Histogram,
}

impl Collector {
    async fn collect(&mut self) {
        let collection_ts = self.start_time_ts.get_or_insert_with(StartTs::new).now();
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
                            start_time_unix_nano: 0,
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
                    MetricValue::Histogram {
                        scale,
                        count,
                        zero_count,
                        sum,
                        min,
                        max,
                        positive,
                        negative,
                    } => OTelMetricData::ExponentialHistogram(ExponentialHistogram {
                        aggregation_temporality: AggregationTemporality::Delta as i32,
                        data_points: vec![ExponentialHistogramDataPoint {
                            attributes,
                            start_time_unix_nano: 0,
                            time_unix_nano: collection_ts,
                            exemplars: Vec::new(),
                            flags: 0,
                            count,
                            scale,
                            sum: Some(sum),
                            zero_count,
                            min: Some(min),
                            max: Some(max),
                            positive: Some(Buckets {
                                offset: positive.0,
                                bucket_counts: positive.1,
                            }),
                            negative: Some(Buckets {
                                offset: negative.0,
                                bucket_counts: negative.1,
                            }),
                            // TODO(rossdylan): wtf do I do here?
                            zero_threshold: 0.0,
                        }],
                    }),
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
        // TODO(rossdylan): Use the duration histogram to see if we need to
        // parallelize the collection loop using rayon or something.
        let collection_dur = collection_start.elapsed();
        tracing::debug!(message="collected metrics", collection_ts=collection_ts, duration=?collection_dur, metrics=otel_metrics.len());

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
        self.export_latency_ms.record_duration_ms(collection_dur);
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
            export_latency_ms: metrics::EXPORT_LATENCY_MS.must(&[]),
        };
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
                let metric = R::must();
                ve.insert(MetricMetadata::new(name, tags, metric.clone()));
                metric
            }
        }
    }
}

pub(crate) mod metrics {
    use crate::{GaugeDef, HistogramDef};

    pub const REGISTERED_METRICS: GaugeDef = GaugeDef::new("metrics64/registry/metrics", &[]);
    pub const EXPORT_LATENCY_MS: HistogramDef =
        HistogramDef::new("metrics64/registry/export_latency_ms", &[]);
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
