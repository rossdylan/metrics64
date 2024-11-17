//! Provide simple tokio runtime metrics. We do this by default in metrics64 to
//! give folks useful stats for "free". Just use our crate and get everything
//! you need.

use std::time::Duration;

mod metrics {
    use crate::GaugeDef;

    pub const TOKIO_ALIVE_TASKS: GaugeDef = GaugeDef::new("tokio/tasks", &["state"]);
    pub const TOKIO_WORKERS: GaugeDef = GaugeDef::new("tokio/workers", &[]);
    pub const TOKIO_GLOBAL_QUEUE_DEPTH: GaugeDef = GaugeDef::new("tokio/global_queue_depth", &[]);
}

/// Export tokio runtime metrics every 10 seconds
pub async fn export_task() {
    let mut ticker = tokio::time::interval(Duration::from_secs(10));
    let alive_tasks = metrics::TOKIO_ALIVE_TASKS.must(&[("state", "alive")]);
    let workers = metrics::TOKIO_WORKERS.must(&[]);
    let queue_depth = metrics::TOKIO_GLOBAL_QUEUE_DEPTH.must(&[]);
    loop {
        ticker.tick().await;
        let handle = tokio::runtime::Handle::current();
        let metrics = handle.metrics();
        alive_tasks.set(metrics.num_alive_tasks() as i64);
        workers.set(metrics.num_workers() as i64);
        queue_depth.set(metrics.global_queue_depth() as i64);
    }
}
