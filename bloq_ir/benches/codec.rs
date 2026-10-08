//! Benchmark saved artifacts, keeping file I/O outside the measured closure.
//! Set BLOQ_IR_CODEC_INPUTS to a platform-separated list of binary file paths.

use std::{hint::black_box, time::Duration};

use bloq_ir::Bloq;
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

fn bench(c: &mut Criterion) {
    let Some(inputs) = std::env::var_os("BLOQ_IR_CODEC_INPUTS") else {
        return;
    };
    let mut group = c.benchmark_group("ir-codec");
    group
        .sample_size(20)
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2));
    for path in std::env::split_paths(&inputs) {
        let name = path.display().to_string();
        let binary = std::fs::read(&path).expect("read benchmark artifact");
        let program = Bloq::from_binary(&binary).expect("decode benchmark artifact");
        group.bench_function(BenchmarkId::new("encode", &name), |b| {
            b.iter(|| black_box(black_box(&program).to_binary()));
        });
        group.bench_function(BenchmarkId::new("decode", &name), |b| {
            b.iter(|| {
                black_box(
                    Bloq::from_binary(black_box(&binary))
                        .expect("the same immutable artifact decoded before benchmarking"),
                )
            });
        });
        group.bench_function(BenchmarkId::new("validate", &name), |b| {
            b.iter(|| {
                black_box(&program)
                    .validate()
                    .expect("the benchmark artifact must pass IR validation")
            });
        });
        group.bench_function(BenchmarkId::new("snapshot", &name), |b| {
            b.iter(|| black_box(black_box(&program).clone()));
        });
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
