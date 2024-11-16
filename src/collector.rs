use std::time::{Instant, SystemTime, UNIX_EPOCH};

const NANOS_PER_SEC: u64 = 1_000_000_000;

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
use tonic::transport::Channel;

use crate::{metrics::MetricValue, registry::Registry};

pub(crate) mod metrics {
    use crate::{GaugeDef, HistogramDef};

    pub const REGISTERED_METRICS: GaugeDef = GaugeDef::new("metrics64/registry/metrics", &[]);
    pub const EXPORT_LATENCY_MS: HistogramDef =
        HistogramDef::new("metrics64/registry/export_latency_ms", &[]);
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
pub struct Collector {
    registry: &'static Registry,
    client: MetricsServiceClient<Channel>,
    start_time_ts: Option<StartTs>,
    metrics_gauge: crate::Gauge,
    export_latency_ms: crate::Histogram,
}

impl Collector {
    pub fn new(registry: &'static Registry, client: MetricsServiceClient<Channel>) -> Self {
        Self {
            registry,
            client,
            start_time_ts: None,
            metrics_gauge: metrics::REGISTERED_METRICS.must(&[]),
            export_latency_ms: metrics::EXPORT_LATENCY_MS.must(&[]),
        }
    }

    pub async fn collect(&mut self) {
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
