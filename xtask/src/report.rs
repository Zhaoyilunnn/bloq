//! Benchmark report generation.
//!
//! Turns raw Criterion output (`target/criterion/**/new/{benchmark,estimates}.json`)
//! and per-case profile artifacts (`target/benchmark/profiles/<stage>/<slug>-d<N>/`)
//! into a browsable HTML table at `target/benchmark/index.html`, plus a
//! machine-readable `target/benchmark/data.json` for agents and tooling.
//!
//! The join key between a benchmark row and its profile directory is the case
//! slug produced by [`bloq_test::benchmark::case_slug`] together with the code
//! distance: `target/benchmark/profiles/<stage>/<case_slug>-d<distance>/`. The
//! aggregate rows retain the workload query in their slug and benchmark id;
//! only the default bench-core suite uses the profile harness's `all` directory.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;
use std::process::Command;

use color_eyre::eyre::{Result, WrapErr, ensure};
use serde_json::{Value, json};

use crate::html::escape;

/// One benchmark result row, classified from Criterion's on-disk metadata.
#[derive(Debug)]
struct BenchRow {
    /// Pipeline stage: `compile` (graph -> Bloq) or `stim` (Bloq -> Stim text).
    stage: String,
    /// Fixture this case derives from, or `combined` for whole-suite rows.
    fixture: String,
    /// Case slug or aggregate workload name.
    case_slug: String,
    /// Full Criterion benchmark ID, useful for re-running a single benchmark.
    bench_id: String,
    distance: Option<u32>,
    mean_ns: f64,
    median_ns: f64,
    std_dev_ns: f64,
    /// Mean from the previous generated target/benchmark/data.json, when present.
    previous_mean_ns: Option<f64>,
    /// Relative mean change vs the previous generated target/benchmark/data.json.
    change: Option<f64>,
    /// Paths relative to `target/benchmark/`, present only if the file exists.
    flamegraph: Option<String>,
    perf_report: Option<String>,
    samply_profile: Option<String>,
}

/// Regenerates local `target/benchmark/index.html` and `data.json` from the
/// latest Criterion output and profile artifacts.
///
/// # Errors
///
/// Returns an error if no Criterion output exists, it contains no readable
/// results, or any report file cannot be written.
pub(crate) fn generate() -> Result<()> {
    // The report tool is repo-local, so resolving the workspace root from the
    // crate's compile-time location is reliable regardless of the cwd that
    // `cargo run` was invoked from.
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf();
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("target"));
    let criterion_dir = target_dir.join("criterion");
    let benchmark_dir = workspace_root.join("target/benchmark");

    ensure!(
        criterion_dir.is_dir(),
        "no Criterion output at {}; run `just bench-cases` (and/or `just bench`) first",
        criterion_dir.display()
    );

    let mut rows = Vec::new();
    collect_rows(&criterion_dir, &benchmark_dir, &mut rows)
        .wrap_err("collect benchmark rows from Criterion output")?;
    ensure!(
        !rows.is_empty(),
        "Criterion output at {} contained no readable benchmark results",
        criterion_dir.display()
    );
    rows.sort_by(|a, b| {
        (&a.fixture, &a.case_slug, &a.stage, a.distance).cmp(&(
            &b.fixture,
            &b.case_slug,
            &b.stage,
            b.distance,
        ))
    });

    fs::create_dir_all(&benchmark_dir)
        .wrap_err_with(|| format!("create {}", benchmark_dir.display()))?;

    let data_path = benchmark_dir.join("data.json");
    apply_previous_report_deltas(&data_path, &mut rows);

    let generated_at = command_stdout("date", &["+%Y-%m-%d %H:%M:%S %Z"]);
    let git_commit = command_stdout("git", &["rev-parse", "--short", "HEAD"]);

    fs::write(
        &data_path,
        render_data_json(&rows, &generated_at, &git_commit),
    )
    .wrap_err_with(|| format!("write {}", data_path.display()))?;

    let html_path = benchmark_dir.join("index.html");
    fs::write(&html_path, render_html(&rows, &generated_at, &git_commit))
        .wrap_err_with(|| format!("write {}", html_path.display()))?;

    let with_profiles = rows.iter().filter(|row| row.flamegraph.is_some()).count();
    println!(
        "wrote {} ({} rows, {} with flamegraphs) and {}",
        html_path.display(),
        rows.len(),
        with_profiles,
        data_path.display()
    );
    Ok(())
}

// ------------------------------------------------------------------------------
// Criterion output collection
// ------------------------------------------------------------------------------

/// Recursively walks `target/criterion` looking for `new/benchmark.json`
/// leaves. Walking instead of reconstructing paths keeps us independent of how
/// Criterion sanitizes benchmark IDs into directory names.
fn collect_rows(dir: &Path, benchmark_dir: &Path, rows: &mut Vec<BenchRow>) -> Result<()> {
    let benchmark_json = dir.join("new/benchmark.json");
    if benchmark_json.is_file() {
        if let Some(row) = parse_row(dir, benchmark_dir)
            .wrap_err_with(|| format!("parse Criterion result at {}", dir.display()))?
        {
            rows.push(row);
        }
        return Ok(());
    }

    for entry in fs::read_dir(dir).wrap_err_with(|| format!("read dir {}", dir.display()))? {
        let path = entry?.path();
        // `report` directories hold Criterion's own HTML output, never results.
        if path.is_dir() && path.file_name().is_some_and(|name| name != "report") {
            collect_rows(&path, benchmark_dir, rows)?;
        }
    }
    Ok(())
}

/// Parses one Criterion benchmark directory into a row, or `None` for groups
/// this report does not know how to classify.
fn parse_row(dir: &Path, benchmark_dir: &Path) -> Result<Option<BenchRow>> {
    let benchmark: Value = read_json(&dir.join("new/benchmark.json"))?;
    let estimates: Value = read_json(&dir.join("new/estimates.json"))?;

    let group_id = benchmark["group_id"].as_str().unwrap_or_default();
    let function_id = benchmark["function_id"].as_str().unwrap_or_default();
    let value_str = benchmark["value_str"].as_str().unwrap_or_default();

    // Classify the row from the group naming scheme set up in the bench
    // harnesses. Unknown groups are skipped rather than failing the report so
    // unrelated benchmarks can coexist in target/criterion.
    let (stage, fixture, case_slug) = if let Some(fixture) = group_id.strip_prefix("compile-case/")
    {
        ("compile", fixture, function_id)
    } else if let Some(fixture) = group_id.strip_prefix("backend-case/") {
        ("stim", fixture, function_id)
    } else if matches!(group_id, "compile" | "backend") {
        (
            if group_id == "compile" {
                "compile"
            } else {
                "stim"
            },
            "combined",
            value_str
                .rsplit_once("-d")
                .map_or("all", |(query, _)| query),
        )
    } else {
        return Ok(None);
    };
    let distance = parse_distance(value_str);
    // The profile harness names its default bench-core workload `all`.
    // Other aggregate queries must never pick up that workload's profiles.
    let profile_slug = if matches!(group_id, "compile" | "backend")
        && distance.is_some_and(|distance| {
            value_str == bloq_test::benchmark::benchmark_value(None, distance)
        }) {
        "all"
    } else {
        case_slug
    };

    let point = |metric: &str| {
        estimates[metric]["point_estimate"]
            .as_f64()
            .unwrap_or(f64::NAN)
    };
    let bench_id = match benchmark["full_id"].as_str() {
        Some(full_id) => full_id.to_string(),
        None => format!("{group_id}/{function_id}/{value_str}"),
    };

    // Profile artifacts live next to the report; record only what exists so
    // the HTML can distinguish "profiled" rows from "not yet profiled" ones.
    let profile_rel = |file: &str| -> Option<String> {
        let distance = distance?;
        let rel = format!("profiles/{stage}/{profile_slug}-d{distance}/{file}");
        benchmark_dir.join(&rel).is_file().then_some(rel)
    };
    let flamegraph = profile_rel("flamegraph.svg");
    let perf_report = profile_rel("report.txt");
    let samply_profile = profile_rel("profile.json.gz");

    Ok(Some(BenchRow {
        stage: stage.to_string(),
        fixture: fixture.to_string(),
        case_slug: case_slug.to_string(),
        bench_id,
        distance,
        mean_ns: point("mean"),
        median_ns: point("median"),
        std_dev_ns: point("std_dev"),
        previous_mean_ns: None,
        change: None,
        flamegraph,
        perf_report,
        samply_profile,
    }))
}

fn apply_previous_report_deltas(data_path: &Path, rows: &mut [BenchRow]) {
    let Ok(previous) = read_json(data_path) else {
        return;
    };
    let mut previous_means = HashMap::new();
    for row in previous["rows"].as_array().into_iter().flatten() {
        let Some(bench_id) = row["bench_id"].as_str() else {
            continue;
        };
        let Some(mean_ns) = row["mean_ns"].as_f64().filter(|mean| *mean > 0.0) else {
            continue;
        };
        previous_means.insert(bench_id, mean_ns);
    }

    for row in rows {
        let Some(&previous_mean_ns) = previous_means.get(row.bench_id.as_str()) else {
            continue;
        };
        row.previous_mean_ns = Some(previous_mean_ns);
        row.change = row
            .mean_ns
            .is_finite()
            .then_some((row.mean_ns - previous_mean_ns) / previous_mean_ns);
    }
}

fn read_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path).wrap_err_with(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).wrap_err_with(|| format!("parse JSON {}", path.display()))
}

/// Extracts the code distance from value strings like `d11` or `all-d7`.
fn parse_distance(value_str: &str) -> Option<u32> {
    let (_, suffix) = value_str.rsplit_once('d')?;
    suffix.parse().ok()
}

fn command_stdout(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

// ------------------------------------------------------------------------------
// Output rendering
// ------------------------------------------------------------------------------

fn render_data_json(rows: &[BenchRow], generated_at: &str, git_commit: &str) -> String {
    let rows = rows
        .iter()
        .map(|row| {
            json!({
                "stage": row.stage,
                "fixture": row.fixture,
                "case_slug": row.case_slug,
                "bench_id": row.bench_id,
                "distance": row.distance,
                "mean_ns": row.mean_ns,
                "median_ns": row.median_ns,
                "std_dev_ns": row.std_dev_ns,
                "previous_mean_ns": row.previous_mean_ns,
                "change_mean": row.change,
                "flamegraph": row.flamegraph,
                "perf_report": row.perf_report,
                "samply_profile": row.samply_profile,
            })
        })
        .collect::<Vec<_>>();
    let document = json!({
        "generated_at": generated_at,
        "git_commit": git_commit,
        "rows": rows,
    });
    serde_json::to_string_pretty(&document).expect("data document is valid JSON")
}

fn render_html(rows: &[BenchRow], generated_at: &str, git_commit: &str) -> String {
    // Group rows into sections: the whole-suite rows first (the easy
    // aggregate view), then one section per fixture.
    let mut sections = BTreeMap::<&str, Vec<&BenchRow>>::new();
    for row in rows {
        sections.entry(&row.fixture).or_default().push(row);
    }
    let combined = sections.remove("combined");

    let mut html = String::new();
    html.push_str(&format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>bloq benchmark report</title>\n<style>{STYLE}</style>\n</head>\n<body>\n\
         <h1>bloq benchmark report</h1>\n\
         <p class=\"meta\">generated {generated} · commit <code>{commit}</code> · \
         machine-readable data in <a href=\"data.json\"><code>data.json</code></a> · \
         click a row to open its flamegraph</p>\n\
         <input id=\"filter\" type=\"search\" placeholder=\"filter rows (case, stage, distance)\">\n",
        generated = escape(generated_at),
        commit = escape(git_commit),
    ));

    if let Some(combined_rows) = combined {
        html.push_str(&render_section("combined suite", &combined_rows));
    }
    for (fixture, section_rows) in sections {
        html.push_str(&render_section(fixture, &section_rows));
    }

    html.push_str(&format!("<script>{SCRIPT}</script>\n</body>\n</html>\n"));
    html
}

fn render_section(title: &str, rows: &[&BenchRow]) -> String {
    let mut section = format!(
        "<section>\n<h2>{}</h2>\n<table>\n<thead><tr>\
         <th>stage</th><th>case</th><th>d</th>\
         <th class=\"num\">mean</th><th class=\"num\">median</th>\
         <th class=\"num\">std dev</th><th class=\"num\">Δ mean</th>\
         <th>profile</th></tr></thead>\n<tbody>\n",
        escape(title)
    );

    for row in rows {
        let distance = row
            .distance
            .map(|distance| distance.to_string())
            .unwrap_or_else(|| "?".to_string());
        let change = match row.change {
            Some(change) => {
                // Criterion's noise floor is well above 1%; don't color tiny drifts.
                let class = if change > 0.02 {
                    "regress"
                } else if change < -0.02 {
                    "improve"
                } else {
                    "flat"
                };
                format!("<span class=\"{class}\">{:+.1}%</span>", change * 100.0)
            }
            None => "—".to_string(),
        };
        let mut profile_links = Vec::new();
        if let Some(flamegraph) = &row.flamegraph {
            profile_links.push(format!("<a href=\"{}\">flame</a>", escape(flamegraph)));
        }
        if let Some(report) = &row.perf_report {
            profile_links.push(format!("<a href=\"{}\">perf</a>", escape(report)));
        }
        if let Some(samply) = &row.samply_profile {
            // Gzipped Firefox Profiler JSON; view via `samply load <file>`.
            profile_links.push(format!(
                "<a href=\"{}\" title=\"open with `samply load`\">samply</a>",
                escape(samply)
            ));
        }
        let profile = if profile_links.is_empty() {
            "<span class=\"missing\">not profiled</span>".to_string()
        } else {
            profile_links.join(" · ")
        };

        let href_attr = row
            .flamegraph
            .as_ref()
            .map(|flamegraph| format!(" class=\"clickable\" data-href=\"{}\"", escape(flamegraph)))
            .unwrap_or_default();
        section.push_str(&format!(
            "<tr{href_attr} title=\"{bench_id}\">\
             <td>{stage}</td><td><code>{case}</code></td><td>{distance}</td>\
             <td class=\"num\">{mean}</td><td class=\"num\">{median}</td>\
             <td class=\"num\">{std_dev}</td><td class=\"num\">{change}</td>\
             <td>{profile}</td></tr>\n",
            bench_id = escape(&row.bench_id),
            stage = escape(&row.stage),
            case = escape(&row.case_slug),
            mean = fmt_time(row.mean_ns),
            median = fmt_time(row.median_ns),
            std_dev = fmt_time(row.std_dev_ns),
        ));
    }

    section.push_str("</tbody>\n</table>\n</section>\n");
    section
}

fn fmt_time(ns: f64) -> String {
    if !ns.is_finite() {
        "—".to_string()
    } else if ns < 1e3 {
        format!("{ns:.1} ns")
    } else if ns < 1e6 {
        format!("{:.2} µs", ns / 1e3)
    } else if ns < 1e9 {
        format!("{:.2} ms", ns / 1e6)
    } else {
        format!("{:.3} s", ns / 1e9)
    }
}

const STYLE: &str = r#"
body { font: 14px/1.5 system-ui, sans-serif; margin: 2rem auto; max-width: 70rem; padding: 0 1rem; color: #1a1a2e; }
h1 { font-size: 1.5rem; }
h2 { font-size: 1.1rem; margin: 2rem 0 0.5rem; border-bottom: 2px solid #e0e0ef; padding-bottom: 0.25rem; }
.meta { color: #667; }
#filter { width: 100%; padding: 0.5rem; margin: 0.5rem 0 1rem; border: 1px solid #ccd; border-radius: 6px; font: inherit; }
table { border-collapse: collapse; width: 100%; }
th, td { text-align: left; padding: 0.3rem 0.6rem; border-bottom: 1px solid #eef; }
th { background: #f4f4fb; position: sticky; top: 0; }
td.num, th.num { text-align: right; font-variant-numeric: tabular-nums; }
tr.clickable { cursor: pointer; }
tr.clickable:hover { background: #eef4ff; }
code { background: #f4f4f8; padding: 0 0.2em; border-radius: 3px; }
.regress { color: #c0392b; font-weight: 600; }
.improve { color: #1e8449; font-weight: 600; }
.flat { color: #889; }
.missing { color: #aab; font-style: italic; }
"#;

const SCRIPT: &str = r#"
document.querySelectorAll('tr.clickable').forEach(row => {
  row.addEventListener('click', event => {
    if (event.target.closest('a')) return; // explicit links win over row click
    window.open(row.dataset.href, '_blank');
  });
});
const filter = document.getElementById('filter');
filter.addEventListener('input', () => {
  const query = filter.value.toLowerCase();
  document.querySelectorAll('tbody tr').forEach(row => {
    row.style.display = row.textContent.toLowerCase().includes(query) ? '' : 'none';
  });
  document.querySelectorAll('section').forEach(section => {
    const visible = section.querySelectorAll('tbody tr:not([style*="none"])').length;
    section.style.display = visible ? '' : 'none';
  });
});
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aggregate_queries_keep_distinct_baselines_and_profiles() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        fs::create_dir_all(root.join("profiles/compile/all-d7")).unwrap();
        fs::write(root.join("profiles/compile/all-d7/report.txt"), "all cases").unwrap();
        let mut rows = Vec::new();
        for (query, mean) in [("bench-core", 100.0), ("cube-line", 10.0)] {
            let dir = root.join(query);
            fs::create_dir_all(dir.join("new")).unwrap();
            fs::write(
                dir.join("new/benchmark.json"),
                json!({
                    "group_id": "compile",
                    "function_id": "runtime",
                    "value_str": format!("cases-{query}-d7"),
                    "full_id": format!("compile/runtime/cases-{query}-d7"),
                })
                .to_string(),
            )
            .unwrap();
            fs::write(
                dir.join("new/estimates.json"),
                json!({
                    "mean": { "point_estimate": mean },
                    "median": { "point_estimate": mean },
                    "std_dev": { "point_estimate": 1.0 },
                })
                .to_string(),
            )
            .unwrap();
            let row = parse_row(&dir, root).unwrap().unwrap();
            assert_eq!(row.case_slug, format!("cases-{query}"));
            assert_eq!(
                row.perf_report.as_deref(),
                (query == "bench-core").then_some("profiles/compile/all-d7/report.txt"),
                "the profile harness's all workload is exactly the default bench-core suite"
            );
            rows.push(row);
        }

        let mut previous: Value = serde_json::from_str(&render_data_json(&rows, "", "")).unwrap();
        // Old reports collapsed the display slug but kept the full id.
        for row in previous["rows"].as_array_mut().unwrap() {
            row["case_slug"] = json!("all");
        }
        let path = root.join("previous.json");
        fs::write(&path, previous.to_string()).unwrap();
        apply_previous_report_deltas(&path, &mut rows);
        assert_eq!(rows[0].previous_mean_ns, Some(100.0));
        assert_eq!(rows[1].previous_mean_ns, Some(10.0));
        assert!(rows.iter().all(|row| row.change == Some(0.0)));
    }
}
