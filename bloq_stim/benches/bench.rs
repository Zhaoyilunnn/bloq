use std::hint::black_box;
use std::time::Duration;

use bloq_compile::{CompileConfig, CompileContext};
use bloq_graph::BlockGraph;
use bloq_ir::Bloq;
use bloq_stim::{StimEmissionError, emit_bloq_stim};
use bloq_test::benchmark::{
    DEFAULT_COMPILE_DISTANCES, benchmark_case_query_from_env, benchmark_value,
    benchmark_workload_cases, case_bench_distances, fixture_case_groups,
};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

fn bench_backend_scenarios(c: &mut Criterion) {
    let case_query = benchmark_case_query_from_env();

    let mut group = c.benchmark_group("backend");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(6));

    for &distance in DEFAULT_COMPILE_DISTANCES {
        let value = benchmark_value(case_query.as_deref(), distance);
        group.bench_with_input(
            BenchmarkId::new("stim", &value),
            &distance,
            |b, distance| {
                let programs = prepare_backend_programs(case_query.as_deref(), *distance);
                b.iter(|| {
                    black_box(emit_stim_workload(&programs));
                })
            },
        );
    }

    group.finish();
}

// Fine-grained emission benchmarks: one Criterion group per fixture, one
// benchmark per case variant and distance, measuring only `emit_bloq_stim`
// over a precompiled program. Benchmark IDs look like
// `backend-case/<fixture>/<case-slug>/d<distance>`.
fn bench_backend_cases(c: &mut Criterion) {
    let case_query = benchmark_case_query_from_env();
    let groups = fixture_case_groups(case_query.as_deref()).expect("enumerate per-case specs");
    let distances =
        case_bench_distances().expect("BLOQ_BENCH_CASE_DISTANCES must contain unsigned integers");

    for fixture_group in groups {
        let mut group = c.benchmark_group(format!("backend-case/{}", fixture_group.fixture_id));
        group.sample_size(10);
        group.warm_up_time(Duration::from_millis(500));
        group.measurement_time(Duration::from_secs(2));

        for spec in &fixture_group.specs {
            for &distance in &distances {
                let graph = spec.case.build();
                let Some(program) =
                    compile_stim_program(&make_context(distance), &graph, &spec.case_name)
                else {
                    continue;
                };
                group.bench_with_input(
                    BenchmarkId::new(&spec.case_slug, format!("d{distance}")),
                    &program,
                    |b, program| {
                        b.iter(|| {
                            black_box(
                                emit_bloq_stim(program)
                                    .unwrap_or_else(|error| {
                                        panic!("emit Stim {}: {error}", spec.case_name)
                                    })
                                    .len(),
                            )
                        });
                    },
                );
            }
        }

        group.finish();
    }
}

criterion_group!(benches, bench_backend_scenarios, bench_backend_cases);
criterion_main!(benches);

fn prepare_backend_programs(case_query: Option<&str>, distance: u32) -> Vec<Bloq> {
    let workloads = benchmark_workload_cases(case_query)
        .expect("load backend workload")
        .into_iter()
        .map(|case| case.build())
        .collect::<Vec<_>>();
    let ctx = make_context(distance);
    let programs = workloads
        .iter()
        .filter_map(|graph| compile_stim_program(&ctx, graph, "backend workload"))
        .collect::<Vec<_>>();
    assert!(
        !programs.is_empty(),
        "backend workload selects no Stim-supported cases"
    );
    programs
}

// Runtime-control and non-Clifford programs remain compiler benchmarks, but
// the static Stim backend deliberately has no spelling for them.
fn compile_stim_program(ctx: &CompileContext, graph: &BlockGraph, case_name: &str) -> Option<Bloq> {
    let program = ctx
        .compile(graph)
        .unwrap_or_else(|error| panic!("compile {case_name}: {error}"))
        .bloq;
    match emit_bloq_stim(&program) {
        Ok(_) => Some(program),
        Err(StimEmissionError::UnsupportedGate(_) | StimEmissionError::UnsupportedNode(_)) => None,
        Err(error) => panic!("emit Stim {case_name}: {error}"),
    }
}

fn emit_stim_workload(programs: &[Bloq]) -> usize {
    let mut total_len = 0_usize;
    for program in programs {
        total_len += emit_bloq_stim(program)
            .unwrap_or_else(|error| panic!("emit Stim workload: {error}"))
            .len();
    }
    total_len
}

fn make_context(distance: u32) -> CompileContext {
    CompileContext::new(CompileConfig::new(distance))
}
