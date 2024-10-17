use core::f64;
use std::{
    any::Any,
    f64::consts::{LN_2, LOG2_E},
    sync::{Arc, LazyLock},
    time::Duration,
};

use crate::{
    cmath::{frexp, ldexp},
    metrics::MetricValue,
};

use parking_lot::Mutex;

const EXPO_MAX_SCALE: i32 = 20;
const EXPO_MIN_SCALE: i32 = -10;

static BOUNDARY_FACTORS: LazyLock<[f64; 21]> = LazyLock::new(|| {
    [
        ldexp(LN_2, -0),
        ldexp(LN_2, -1),
        ldexp(LN_2, -2),
        ldexp(LN_2, -3),
        ldexp(LN_2, -4),
        ldexp(LN_2, -5),
        ldexp(LN_2, -6),
        ldexp(LN_2, -7),
        ldexp(LN_2, -8),
        ldexp(LN_2, -9),
        ldexp(LN_2, -10),
        ldexp(LN_2, -11),
        ldexp(LN_2, -12),
        ldexp(LN_2, -13),
        ldexp(LN_2, -14),
        ldexp(LN_2, -15),
        ldexp(LN_2, -16),
        ldexp(LN_2, -17),
        ldexp(LN_2, -18),
        ldexp(LN_2, -19),
        ldexp(LN_2, -20),
    ]
});
static SCALE_FACTORS: LazyLock<[f64; 21]> = LazyLock::new(|| {
    [
        ldexp(LOG2_E, 0),
        ldexp(LOG2_E, 1),
        ldexp(LOG2_E, 2),
        ldexp(LOG2_E, 3),
        ldexp(LOG2_E, 4),
        ldexp(LOG2_E, 5),
        ldexp(LOG2_E, 6),
        ldexp(LOG2_E, 7),
        ldexp(LOG2_E, 8),
        ldexp(LOG2_E, 9),
        ldexp(LOG2_E, 10),
        ldexp(LOG2_E, 11),
        ldexp(LOG2_E, 12),
        ldexp(LOG2_E, 13),
        ldexp(LOG2_E, 14),
        ldexp(LOG2_E, 15),
        ldexp(LOG2_E, 16),
        ldexp(LOG2_E, 17),
        ldexp(LOG2_E, 18),
        ldexp(LOG2_E, 19),
        ldexp(LOG2_E, 20),
    ]
});

/// Our histogram will be the log-exponential based histogram defined by otel.
/// Seems tdigest has fallen to the wayside, and the general consensus?? is
/// log-exponential histograms are the thing. I think ddsketch, otel,
/// and prometheus all implement some version of it. I think the implementation
/// should be fairly straight forward, but we'll see. The go for it seems
/// reasonable, at least compared to the weirdness of tdigest
#[derive(Debug)]
struct HistogramInner {
    count: u64,
    min: f64,
    max: f64,
    sum: f64,
    max_size: i64,
    zero_count: u64,
    /// scale describes the resolution of the histogram. Boundaries are
    /// located at powers of the base, where:
    ///
    ///   base = 2 ^ (2 ^ -Scale)
    scale: i32,
    max_scale: i32,
    positive_buckets: HistogramBuckets,
    negative_buckets: HistogramBuckets,
}

impl Default for HistogramInner {
    fn default() -> Self {
        Self::new(160, 20)
    }
}

impl HistogramInner {
    fn new(max_size: i64, max_scale: i32) -> Self {
        Self {
            count: 0,
            sum: 0f64,
            max: f64::MIN,
            min: f64::MAX,
            max_size,
            zero_count: 0,
            scale: max_scale,
            max_scale,
            positive_buckets: HistogramBuckets::default(),
            negative_buckets: HistogramBuckets::default(),
        }
    }

    fn get_and_reset(&mut self) -> MetricValue {
        let value = MetricValue::Histogram {
            scale: self.scale,
            count: self.count,
            zero_count: self.zero_count,
            sum: self.sum,
            min: self.min,
            max: self.max,
            positive: (
                self.positive_buckets.start_bin,
                self.positive_buckets.counts.clone(),
            ),
            negative: (
                self.negative_buckets.start_bin,
                self.negative_buckets.counts.clone(),
            ),
        };
        // now reset the histogram back to its defaults
        // TODO(rossdylan): Do we want to preserve scale across resets to try and avoid thrash?
        self.scale = self.max_scale;
        self.count = 0;
        self.zero_count = 0;
        self.sum = 0f64;
        self.min = f64::MAX;
        self.max = f64::MIN;
        self.positive_buckets.start_bin = 0;
        self.positive_buckets.counts.truncate(0);
        self.negative_buckets.start_bin = 0;
        self.negative_buckets.counts.truncate(0);
        value
    }

    /// Map the given floating point value to a bin in our histogram.
    fn get_bin(&self, value: f64) -> i32 {
        let (frac, exp) = frexp(value);
        let exp = exp as i32;
        if self.scale <= 0 {
            // Because of the choice of fraction is always 1 power of two higher than we want.
            let correction = if frac == 0.5 {
                // If v is an exact power of two the frac will be .5 and the exp
                // will be one higher than we want.
                2
            } else {
                1
            };
            (exp - correction) >> (-self.scale)
        } else {
            (exp << self.scale) + ((frac.ln() * SCALE_FACTORS[self.scale as usize]) as i32) - 1
        }
    }

    /// Return the magnitude of the scale change needed to fit bin in
    /// the bucket. If no scale change is needed 0 is returned
    fn scale_delta(&self, bin: i32, start_bin: i32, length: usize) -> i32 {
        if length == 0 {
            // No need to rescale if there are no buckets.
            return 0;
        }
        let (mut low, mut high) = if start_bin >= bin {
            (bin, start_bin + length as i32 - 1)
        } else {
            (start_bin, bin)
        };
        let mut count = 0i32;
        while (high - low) as i64 >= self.max_size {
            low >>= 1;
            high >>= 1;
            count += 1;
            if count > EXPO_MAX_SCALE - EXPO_MIN_SCALE {
                break;
            }
        }
        count
    }

    /// This is a helper function to determine the boundaries of each bucket in
    /// the histogram. It is taken from the go implementation here:
    /// https://github.com/open-telemetry/opentelemetry-collector/blob/main/exporter/internal/otlptext/databuffer.go#L144
    /// NOTE(rossdylan): The comments in the go source indicate that this is
    /// inaccurate in certain situations, so for anything that needs actual
    /// accuracy (ie, a tsdb impl) we'll need to implement a better way
    fn boundaries(&self) -> Vec<(f64, f64)> {
        let factor = BOUNDARY_FACTORS[self.scale as usize];
        let mut bounds: Vec<(f64, f64)> = Vec::with_capacity(
            self.negative_buckets.counts.len() + self.positive_buckets.counts.len(),
        );
        for index in 0..self.negative_buckets.counts.len() {
            let pos = self.negative_buckets.counts.len() - index - 1;
            let adjusted_index = (self.negative_buckets.start_bin + pos as i32) as f64;
            bounds.push((
                -((factor * adjusted_index).exp()),
                -((factor * (adjusted_index + 1.0)).exp()),
            ))
        }
        for index in 0..self.positive_buckets.counts.len() {
            let adjusted_index = (self.positive_buckets.start_bin + index as i32) as f64;
            bounds.push((
                (factor * adjusted_index).exp(),
                (factor * (adjusted_index + 1.0)).exp(),
            ))
        }
        bounds
    }

    fn record(&mut self, value: f64) {
        self.count += 1;
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }
        self.sum += value;
        let abs_value = value.abs();
        if abs_value == 0.0 {
            self.zero_count += 1;
            return;
        }
        let bin = self.get_bin(abs_value);
        let buckets = if value < 0f64 {
            &self.negative_buckets
        } else {
            &self.positive_buckets
        };
        let scale_delta = self.scale_delta(bin, buckets.start_bin, buckets.counts.len());
        let final_bin = if scale_delta != 0 {
            if self.scale - scale_delta < EXPO_MIN_SCALE {
                // With a scale of -10 there are only two buckets for the whole
                // range of f64 values.
                // This can only happen if there is a max size of 1
                tracing::error!("exponential histogram scale underflow");
                return;
            }
            // Downscale
            self.scale -= scale_delta;
            self.positive_buckets.downscale(scale_delta);
            self.negative_buckets.downscale(scale_delta);
            debug_assert_eq!(
                self.positive_buckets.counts.iter().sum::<u64>()
                    + self.negative_buckets.counts.iter().sum::<u64>()
                    + self.zero_count,
                self.count - 1,
                "combined buckets sum must equal total count"
            );
            self.get_bin(abs_value)
        } else {
            bin
        };
        let buckets = if value < 0f64 {
            &mut self.negative_buckets
        } else {
            &mut self.positive_buckets
        };
        buckets.record(final_bin);
        debug_assert_eq!(
            self.positive_buckets.counts.iter().sum::<u64>()
                + self.negative_buckets.counts.iter().sum::<u64>()
                + self.zero_count,
            self.count,
            "combined buckets sum must equal total count"
        );
    }
}

#[derive(Default, Debug)]
struct HistogramBuckets {
    /// Offset is the bucket index of the first entry in the counts vector.
    start_bin: i32,
    /// counts is a vector where counts[i] carries the count of the bucket at
    /// index (start_bin+i). counts[i] is the count of values greater than
    /// base^(start_bin+i) and less than or equal to base^(start_bin+i+1).
    counts: Vec<u64>,
}

impl HistogramBuckets {
    fn record(&mut self, bin: i32) {
        if self.counts.is_empty() {
            // self.start_bin is an arbitrary value but will always be the lowest
            // value bin. So if we have a fresh hist and get a bin of 20
            // our struct is {start_bin: 20, counts: [1]}
            self.counts.push(1);
            self.start_bin = bin;
            return;
        }
        let end_bin = self.start_bin + self.counts.len() as i32 - 1;
        // if the new bin is inside the current range;
        if bin >= self.start_bin && bin <= end_bin {
            // We've already seen this bin before so we can index directly into
            // the counts vector, using self.start_bin to calculate the actual
            // index.
            self.counts[(bin - self.start_bin) as usize] += 1;
            return;
        }
        // if the new bin is before the current start, expand the vector and
        // shift all of our current bins
        if bin < self.start_bin {
            // We've been given a bin smaller then our current start, which means
            // we need to potentially increase the size of our counts vector and
            // shift our current set of counts
            // {start: 20, counts: {1, 1, 1}, bin: 18, end_bin: (20 + 3 - 1) = 22
            // orig_len: 3
            // new_len: (22 - 18 + 1) = 5
            // shift: 20 - 18 = 2
            // counts.resize(new_len) => {1, 1, 1, 0, 0}
            // counts.copy_within(0..orgig_len, 2) => {1, 1, 1, 1, 1}
            // counts[1..2] = 0 => {0, 0, 1, 1, 1}
            // counts[0] = 1 => {1, 0, 1, 1, 1}
            let orig_len = self.counts.len();
            let new_len = (end_bin - bin + 1) as usize;
            let shift = (self.start_bin - bin) as usize;
            self.counts.resize(new_len, 0);
            // NOTE(rossdylan): The arguments here are flipped from go's copy
            // builtin.
            self.counts.copy_within(0..orig_len, shift);
            for index in 1..shift {
                self.counts[index] = 0;
            }
            self.start_bin = bin;
            self.counts[0] = 1;
            return;
        }
        // If the new bin is after the end, expand the end of the vector
        if bin > end_bin {
            // This is easy, since we don't need to shift our counts around
            self.counts.resize((bin - self.start_bin + 1) as usize, 0);
            self.counts[(bin - self.start_bin) as usize] = 1;
        }
    }

    fn downscale(&mut self, delta: i32) {
        // Example
        // delta = 2
        // Original offset: -6
        // Counts: [ 3,  1,  2,  3,  4,  5, 6, 7, 8, 9, 10]
        // bins:    -6  -5, -4, -3, -2, -1, 0, 1, 2, 3, 4
        // new bins:-2, -2, -1, -1, -1, -1, 0, 0, 0, 0, 1
        // new Offset: -2
        // new Counts: [4, 14, 30, 10]
        if self.counts.len() <= 1 || delta < 1 {
            self.start_bin >>= delta;
            return;
        }
        let steps = 1 << delta;
        let offset = self.start_bin % steps;
        let offset = (offset + steps) % steps; // to make offset positive
        for index in 1..self.counts.len() {
            let idx = index as i32 + offset;
            if idx % steps == 0 {
                self.counts[(idx / steps) as usize] = self.counts[index];
                continue;
            }
            self.counts[(idx / steps) as usize] += self.counts[index];
        }
        let last_idx = (self.counts.len() - 1 + offset as usize) / steps as usize;
        self.counts.truncate(last_idx + 1);
        self.start_bin >>= delta;
    }
}

/// A threadsafe histogram for tracking the distribution of values in an
/// efficient way.
#[derive(Default, Clone, Debug)]
pub struct Histogram {
    inner: Arc<Mutex<HistogramInner>>,
}

impl Histogram {
    pub fn record(&self, value: f64) {
        self.inner.lock().record(value);
    }

    pub fn record_duration_sec(&self, value: Duration) {
        self.inner.lock().record(value.as_secs() as f64);
    }

    pub fn record_duration_ms(&self, value: Duration) {
        self.inner.lock().record(value.as_millis() as f64)
    }
}

impl super::Metric for Histogram {
    fn must() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl super::Recordable for Histogram {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn value(&self) -> MetricValue {
        let mut inner = self.inner.lock();
        tracing::debug!(message="histogram.value", min=inner.min, max=inner.max, sum=inner.sum, count=inner.count, buckets=?inner.boundaries());
        inner.get_and_reset()
    }
}

#[cfg(test)]
mod test {
    use core::f64;

    use super::Histogram;

    #[test]
    fn min_max_float() {
        let hist = Histogram::default();
        hist.record(f64::MIN);
        hist.record(f64::MAX);
        println!("{:?}", hist.inner.lock().boundaries());
    }

    #[test]
    fn increments() {
        let hist = Histogram::default();
        for i in 0..20_000 {
            hist.record(i as f64);
        }
        println!("{:?}", hist.inner.lock().boundaries());
    }

    #[test]
    fn negative_increments() {
        let hist = Histogram::default();
        for i in -20_000..0 {
            hist.record(i as f64);
        }
        println!("{:?}", hist.inner.lock().boundaries());
    }
}
