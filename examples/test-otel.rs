use std::time::{Duration, Instant};

use metrics64::{CounterDef, HistogramDef};

const METRIC: CounterDef = CounterDef::new("metrics64/test_counter", &["test"]);
const WAIT_METRIC: HistogramDef = HistogramDef::new("metrics64/test_latency_ms", &[]);
#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    metrics64::DEFAULT_REGISTRY.start("test-otel");
    let counter = METRIC.must(&[("test", "value")]);
    let hist = WAIT_METRIC.must(&[]);
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    loop {
        let loop_start = Instant::now();
        counter.incr();
        ticker.tick().await;
        hist.record_duration_ms(loop_start.elapsed());
    }
}
