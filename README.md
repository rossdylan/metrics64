# Metrics64

Realistically, this is a metrics library just for me (Ross).

It is **not**:
* A generic facade for metrics
* Designed for maximum performance
* Designed for minimum memory usage

It **is**
* A pretty easy to use otel implemenation
* Designed to be dead simple to add new metrics
## API
```rust
use std::time::{Duration, Instant};
use metrics64::{GaugeDef, CounterDef, HistogramDef};

const METRIC: CounterDef = CounterDef::new("metrics64/test_counter", &["test"]);
const GAUGE: GaugeDef = GaugeDef::new("metrics64/test_gague", &["other_test"])
const WAIT_METRIC: HistogramDef = HistogramDef::new("metrics64/test_latency_ms", &[]);

#[tokio::main]
pub async fn main() -> anyhow::Result<()> {
    metrics64::DEFAULT_REGISTRY.start();
    let counter = METRIC.must(&[("test", "value")]);
    let gauge = GAUGE.must(&[("other_test", "other_value")]);
    gauge.set(100);

    let hist = WAIT_METRIC.must(&[]);
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    loop {
        let loop_start = Instant::now();
        counter.incr();
        ticker.tick().await;
        hist.record_duration_ms(loop_start.elapsed());
    }
}
```

# Context

I spent 6-ish years at a previous company that had a really easy to use metrics
library. It wasn't the fastest implementation, and it wasn't generic at all. It
did, however _just work_, and made it dead simple to add new metrics to your
software.

Since I've left that job I've thought a lot about that library. Especially as I
struggle to get that same ease of use in both my personal and profession work.
Metrics64 is my attempt to make a rust metrics library that I actaully want to
use.

# Core Ideas

1. **There are no knobs.** I don't want to have a ton of boiler plate to configure.
    1. Its just otel. Open Telemetry is a large consensus driven project, so naturally I'm not stoked about how complex their api is. BUT their api is push based and has good support for log-exponential histograms.
    2. Only provide a single pretty good option for Counters, Gauges, and Histograms
2. It **must** support some kind of dynamic histogram.
   1. The library I used at my previous job had a tdigest implementation built in. It was the greatest thing since sliced bread. I never want to think about choosing histogram buckets, or how to aggregate that data across machines.
3. **A minimum of _type level magic_.** Just enough to keep things pleasant to use, but not enough to require a PHD to understand.
4. **Creation of a new metric can be relatively slow**, but usage of said metric can't

# Benchmarks
These are worthless, its just running on my M2 Macbook Air.

```
must-1-tag-new          time:   [444.45 ns 449.38 ns 455.69 ns]
must-1-tag-existing     time:   [48.223 ns 48.274 ns 48.330 ns]
must-8-tags             time:   [332.76 ns 333.36 ns 333.93 ns]
must-9-tags             time:   [441.40 ns 442.30 ns 443.37 ns]
counter-incr-1-tag      time:   [1.9861 ns 1.9907 ns 1.9956 ns]
counter-clone-1-tag     time:   [3.7396 ns 3.7440 ns 3.7493 ns]
histogram-incr-1-tag    time:   [9.0636 ns 9.0749 ns 9.0865 ns]
```
