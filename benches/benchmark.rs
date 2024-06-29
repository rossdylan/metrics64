use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pprof::criterion::{Output, PProfProfiler};

use metrics64::CounterDef;

pub fn benchmark_must(c: &mut Criterion) {
    const DEF: CounterDef =
        CounterDef::new("metrics64/benchmarks/one-tag", metrics64::Target::Pod, &["tag"]);
    c.bench_function("must-1-tag-new", |b| {
        let mut values = (0i64..).map(|i| Box::leak(i.to_string().into_boxed_str()));
        b.iter(|| DEF.must(&[("tag", values.next().unwrap())]))
    });
    c.bench_function("must-1-tag-existing", |b| {
        let _counter = black_box(DEF.must(&[("tag", "one")]));
        b.iter(|| DEF.must(&[("tag", "one")]));
    });
}

pub fn benchmark_counter(c: &mut Criterion) {
    const DEF: CounterDef = CounterDef::new(
        "metrics64/benchmarks/counter/incr-one-tag",
        metrics64::Target::Pod,
        &["tag"],
    );
    c.bench_function("counter-incr-1-tag", |b| {
        let counter = DEF.must(&[("tag", "one")]);
        b.iter(|| counter.incr());
    });
    c.bench_function("counter-clone-1-tag", |b| {
        let counter = DEF.must(&[("tag", "one")]);
        b.iter(|| black_box(counter.clone()));
    });
}

//pub fn profile_observe(c: &mut Criterion) {
//    c.bench_function("profile-observe", |b| {
//        let mut t = <TDigest>::new(100.0);
//        let mut values = 1i64..;
//
//        b.iter(|| t.observe(black_box(values.next().unwrap())))
//    });
//}

//criterion_group! {
//    name = profiles;
//    config = Criterion::default().with_profiler(PProfProfiler::new(100, Output::Flamegraph(None)));
//    targets = profile_observe, profile_merge_bytes
//}
criterion_group!(benches, benchmark_must, benchmark_counter);
criterion_main!(benches);
