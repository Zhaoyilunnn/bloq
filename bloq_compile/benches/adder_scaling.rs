use std::{hint::black_box, time::Duration};

use bloq_compile::{CompileConfig, CompileContext};
use bloq_test::benchmark::controlled_adder;
use criterion::{BatchSize, BenchmarkId, Criterion, SamplingMode, criterion_group, criterion_main};

fn bench(c: &mut Criterion) {
    let sizes = std::env::var("BLOQ_ADDER_BITS").unwrap_or_else(|_| "3,8,16,33".into());
    let distances = std::env::var("BLOQ_ADDER_DISTANCES").unwrap_or_else(|_| "3".into());
    let mut group = c.benchmark_group("adder-scaling");
    group
        .sample_size(10)
        .sampling_mode(SamplingMode::Flat)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_secs(2));
    for bits in sizes
        .split(',')
        .map(|n| n.trim().parse::<usize>().expect("bit count"))
    {
        assert!(bits >= 3, "benchmark width must be at least three bits");
        for distance in distances
            .split(',')
            .map(|n| n.trim().parse::<u32>().expect("distance"))
        {
            group.bench_function(BenchmarkId::new(format!("d{distance}"), bits), |b| {
                let program = controlled_adder(bits);
                b.iter_batched(
                    || CompileContext::new(CompileConfig::new(distance)),
                    |context| black_box(context.compile(&program).expect("compile adder")),
                    BatchSize::PerIteration,
                );
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
