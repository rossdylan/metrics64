use std::time::Duration;

use metrics64::CounterDef;

const METRIC: CounterDef =
    CounterDef::new("metrics64/test_counter", metrics64::Target::Pod, &["test"]);
#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    metrics64::DEFAULT_REGISTRY.start();
    let counter = METRIC.must(&[("test", "value")]);
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    loop {
        counter.incr();
        ticker.tick().await;
    }
    Ok(())
}
