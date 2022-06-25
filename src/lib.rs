/// The api I'm thinking about here is a cross between  Google's Monarch and Dropbox's vortex2
/// let push_counter_def = metrics64::DefineCounter(
///     "metrics64/exporter/pushes",    // Metric name
///     metrics64::targets::Pod,       // target metric (the location this metric is exported from)
///     &["status"],                // tags associated with this metric
/// );
/// let push_counter = push_counter_def.new("status", "ok");

/// A trait that defines a set of global location tags that are evalulated once per process
/// to provide
pub trait Target {
    fn tags() -> &'static [(&'static str, &'static str)];
}

pub struct CounterDef {}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
