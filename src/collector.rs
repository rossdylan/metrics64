use std::{
    env,
    sync::OnceLock,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use gethostname::gethostname;
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
    resource::v1::Resource,
};
use tonic::transport::Channel;
use uuid::Uuid;

use crate::{metrics::MetricValue, registry::Registry};

pub(crate) mod metrics {
    use crate::{GaugeDef, HistogramDef};

    /// A gauge tracking how many metrics we have registered in this process
    pub const REGISTERED_METRICS: GaugeDef = GaugeDef::new("metrics64/registry/metrics", &[]);

    /// A histogram tracking the latency of our export calls.
    pub const EXPORT_LATENCY_MS: HistogramDef =
        HistogramDef::new("metrics64/registry/export_latency_ms", &[]);
}

/// A struct containing all of our process global tags. These are discovered
/// lazily at first export.
pub struct ProcessAttributes {
    /// The hostname of the node running this process
    pub hostname: String,
    /// The binary name for this process
    pub command: Option<String>,
    /// A unique uuid assigned at process start. This is an otel-ism which is
    /// used to track process state in the otel collector
    pub instance: Uuid,
}

impl ProcessAttributes {
    /// Cache our hostname and other 'static' process information using a [`OnceLock`]
    // so we don't call things like [`gethostname`] a bunch.
    pub fn get() -> &'static Self {
        static PROC_ATTR_ONCE: OnceLock<ProcessAttributes> = OnceLock::new();
        PROC_ATTR_ONCE.get_or_init(|| ProcessAttributes {
            hostname: gethostname()
                .into_string()
                .expect("gethostname should return a valid utf8 string"),
            command: env::args().next(),
            // TODO(rossdylan): I'm not sold on this whole instance-id thing.
            instance: Uuid::new_v4(),
        })
    }
}

/// We create a static set of otel resource attributes that will be cached for
/// the lifetime of this process.
/// A lot of this is just following the otel tag conventions to ensure we gather
/// data in a compatible way.
/// TODO(rossdylan): We'll need to expand this to kube and containers at some point
pub fn resource_attributes(service: Option<&str>) -> Vec<KeyValue> {
    let proc_attr = ProcessAttributes::get();
    let svc_name = match (service, proc_attr.command.as_deref()) {
        (Some(svc), _) => svc.to_string(),
        (None, Some(cli)) => format!("unknown_service:{cli}"),
        (None, None) => "unknown_service".to_string(),
    };
    let mut attrs = vec![
        KeyValue {
            key: "service.name".to_string(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(svc_name)),
            }),
        },
        KeyValue {
            key: "service.instance.id".to_string(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(proc_attr.instance.to_string())),
            }),
        },
        KeyValue {
            key: "host.name".to_string(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(proc_attr.hostname.clone())),
            }),
        },
        KeyValue {
            key: "telemetry.sdk.language".to_string(),
            value: Some(AnyValue {
                value: Some(Value::StringValue("rust".to_string())),
            }),
        },
        KeyValue {
            key: "telemetry.sdk.name".to_string(),
            value: Some(AnyValue {
                value: Some(Value::StringValue("metrics64".to_string())),
            }),
        },
    ];
    if let Some(cli) = &proc_attr.command {
        attrs.push(KeyValue {
            key: "process.executable.name".to_string(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(cli.clone())),
            }),
        })
    }
    attrs
}

/// The collector is used to run our periodic exports to some endpoint that
/// speaks otel.
pub struct Collector {
    registry: &'static Registry,
    client: MetricsServiceClient<Channel>,
    resource_attrs: Vec<KeyValue>,
    metrics_gauge: crate::Gauge,
    export_latency_ms: crate::Histogram,
}

impl Collector {
    pub fn new(
        service: Option<&str>,
        registry: &'static Registry,
        client: MetricsServiceClient<Channel>,
    ) -> Self {
        Self {
            registry,
            client,
            resource_attrs: resource_attributes(service),
            metrics_gauge: metrics::REGISTERED_METRICS.must(&[]),
            export_latency_ms: metrics::EXPORT_LATENCY_MS.must(&[]),
        }
    }

    pub async fn collect(&mut self) {
        let collection_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time rewound before unix epoch")
            .as_nanos() as u64;
        let collection_start = Instant::now();
        let otel_metrics = {
            let metrics = self.registry.metrics.read();
            let mut otel_metrics = Vec::with_capacity(metrics.len());
            self.metrics_gauge.set(metrics.len() as i64);
            for metric in metrics.values() {
                let start_ts = *metric.export_start_ts.get_or_init(|| collection_ts);
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
                            start_time_unix_nano: start_ts,
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
                            start_time_unix_nano: start_ts,
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
                    resource: Some(Resource {
                        attributes: self.resource_attrs.clone(),
                        dropped_attributes_count: 0,
                    }),
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
