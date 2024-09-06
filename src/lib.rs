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

mod cmath;
mod metrics;
mod registry;

use metrics::{counter::Counter, gauge::Gauge, histogram::Histogram, MetricDef};

pub use metrics::Target;
pub use registry::DEFAULT_REGISTRY;

pub type CounterDef = MetricDef<Counter>;
pub type GaugeDef = MetricDef<Gauge>;
pub type HistogramDef = MetricDef<Histogram>;
