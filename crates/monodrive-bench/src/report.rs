//! `results.json` as one HTML page: inline CSS and SVG, no scripts, nothing
//! fetched. It reads only that file, the budgets and an optional saved
//! baseline, so it can't show a number a run didn't produce. Hover values
//! come from SVG titles.

use crate::budgets::{self, Budget, Status};
use crate::format::{Bench, Cache, Results, Series, SuiteRun};
use crate::stats::{self, Verdict};
use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::{Path, PathBuf};

const STYLE: &str = r#"
:root {
  color-scheme: light;
  --bg: #f6f6f4; --surface: #fcfcfb; --rule: #e2e1dc; --grid: #ecebe7;
  --fg: #0b0b0b; --fg-2: #2b2a28; --muted: #52514e; --dim: #75746f;
  --s1: #2a78d6; --s2: #eb6834; --s3: #1baf7a;
  --good: #008300; --bad: #c62f2f;
  --font: -apple-system, BlinkMacSystemFont, "Inter", system-ui, sans-serif;
  --mono: ui-monospace, "SF Mono", Menlo, monospace;
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    color-scheme: dark;
    --bg: #111110; --surface: #1a1a19; --rule: #2e2d2b; --grid: #242422;
    --fg: #f0efec; --fg-2: #dddcd6; --muted: #c3c2b7; --dim: #8d8c85;
    --s1: #3987e5; --s2: #d95926; --s3: #199e70;
    --good: #4fb84f; --bad: #e66767;
  }
}
:root[data-theme="dark"] {
  color-scheme: dark;
  --bg: #111110; --surface: #1a1a19; --rule: #2e2d2b; --grid: #242422;
  --fg: #f0efec; --fg-2: #dddcd6; --muted: #c3c2b7; --dim: #8d8c85;
  --s1: #3987e5; --s2: #d95926; --s3: #199e70;
  --good: #4fb84f; --bad: #e66767;
}
* { box-sizing: border-box; }
body { margin: 0; padding: 32px clamp(16px, 4vw, 56px) 64px; background: var(--bg); color: var(--fg); font: 14px/1.5 var(--font); }
h1 { font-size: clamp(26px, 4vw, 38px); letter-spacing: -.02em; margin: 0 0 6px; }
h2 { font-size: 22px; margin: 40px 0 4px; letter-spacing: -.01em; }
h3 { font-size: 15px; margin: 0 0 2px; }
.lede, .sub { color: var(--muted); margin: 0 0 16px; max-width: 80ch; }
.sub { font-size: 13px; margin-bottom: 10px; }
.panel { background: var(--surface); border: 1px solid var(--rule); border-radius: 8px; padding: 16px; margin: 0 0 16px; }
.noise { border-color: var(--bad); }
.noise h3 { color: var(--bad); }
.noise ul { margin: 6px 0 0; padding-left: 20px; }
.figure { display: grid; grid-template-columns: minmax(0, 1fr); gap: 12px; }
.figure svg { max-width: 760px; }
svg { display: block; width: 100%; height: auto; font: 11px var(--font); }
svg text { fill: var(--muted); }
svg .label { fill: var(--fg-2); }
svg .grid { stroke: var(--grid); }
svg .axis { stroke: var(--rule); }
svg .hit { fill: transparent; }
svg .hit:hover { fill: var(--grid); }
.legend { display: flex; flex-wrap: wrap; gap: 4px 14px; margin: 0 0 6px; font-size: 12px; color: var(--muted); }
.legend span::before { content: ""; display: inline-block; width: 10px; height: 10px; border-radius: 50%; margin-right: 5px; vertical-align: -1px; background: var(--c); }
.scroll { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-size: 12px; font-variant-numeric: tabular-nums; }
th { color: var(--dim); font-weight: 500; text-align: right; padding: 6px 8px; border-bottom: 1px solid var(--rule); white-space: nowrap; }
td { text-align: right; padding: 6px 8px; border-bottom: 1px solid var(--grid); white-space: nowrap; color: var(--fg-2); }
th:first-child, td:first-child { text-align: left; white-space: normal; }
td.series { text-align: left; }
td small { display: block; color: var(--dim); }
.faster, .within { color: var(--good); }
.slower, .over { color: var(--bad); }
.flat, .missing { color: var(--dim); }
dl { display: grid; grid-template-columns: max-content 1fr; gap: 2px 14px; margin: 0; font-size: 12px; }
dt { color: var(--dim); }
dd { margin: 0; overflow-wrap: anywhere; }
.foot { color: var(--dim); font-size: 12px; margin-top: 32px; max-width: 90ch; }
"#;

const COLORS: [&str; 3] = ["var(--s1)", "var(--s2)", "var(--s3)"];

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A time at the precision its size deserves.
pub fn ms(value: f64) -> String {
    if !value.is_finite() {
        "–".into()
    } else if value == 0. {
        "0".into()
    } else if value < 0.1 {
        format!("{:.1} µs", value * 1e3)
    } else if value < 10. {
        format!("{value:.2} ms")
    } else if value < 1_000. {
        format!("{value:.1} ms")
    } else {
        format!("{:.2} s", value / 1e3)
    }
}

fn times(value: f64) -> String {
    if value < 10. {
        format!("{value:.2}")
    } else {
        format!("{value:.0}")
    }
}

/// A verdict as words. "No clear difference" never carries a multiplier.
pub fn verdict(verdict: Verdict) -> (String, &'static str) {
    match verdict {
        Verdict::Faster { low, high } => (
            format!("faster, {}× to {}×", times(low), times(high)),
            "faster",
        ),
        Verdict::Slower { low, high } => (
            format!("slower, {}× to {}×", times(low), times(high)),
            "slower",
        ),
        Verdict::NoClearDifference => ("no clear difference".into(), "flat"),
        Verdict::TooFewSamples => ("too few samples".into(), "flat"),
    }
}

struct Summary {
    n: usize,
    median: f64,
    min: f64,
    p90: f64,
}

fn summary(series: &Series) -> Option<Summary> {
    let wall = series.wall();
    (!wall.is_empty()).then(|| Summary {
        n: wall.len(),
        median: stats::median(&wall),
        min: wall.iter().copied().fold(f64::INFINITY, f64::min),
        p90: stats::percentile(&wall, 0.9),
    })
}

fn median_of(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let values: Vec<f64> = values.flatten().collect();
    (!values.is_empty()).then(|| stats::median(&values))
}

/// One row of a chart: a series of one bench.
struct Row<'a> {
    label: String,
    series_name: String,
    color: usize,
    summary: Summary,
    series: &'a Series,
    bench: &'a Bench,
}

/// A horizontal range chart: a line from the minimum to p90 and a dot at the
/// median, one axis, linear unless the values span more than 30 times.
fn chart(rows: &[Row]) -> (String, bool) {
    const WIDTH: f64 = 760.;
    const LABELS: f64 = 230.;
    const ROW: f64 = 22.;
    const TOP: f64 = 6.;
    let lowest = rows
        .iter()
        .map(|r| r.summary.min)
        .filter(|v| *v > 0.)
        .fold(f64::INFINITY, f64::min);
    let highest = rows.iter().map(|r| r.summary.p90).fold(0., f64::max);
    let log = highest / lowest > 30.;
    let (from, to) = if log {
        (lowest / 1.3, highest * 1.3)
    } else {
        (0., highest * 1.08)
    };
    let plot = WIDTH - LABELS - 12.;
    let x = |v: f64| {
        let share = if log {
            (v.max(from).ln() - from.ln()) / (to.ln() - from.ln())
        } else {
            v / to
        };
        LABELS + share.clamp(0., 1.) * plot
    };
    let height = TOP + rows.len() as f64 * ROW + 24.;
    let mut svg = format!(
        r#"<svg viewBox="0 0 {WIDTH} {height}" role="img" aria-label="Median, minimum and p90 of each result">"#
    );
    let ticks: Vec<f64> = if log {
        let mut tick = 10f64.powf(from.log10().floor());
        let mut ticks = Vec::new();
        while tick <= to {
            if tick >= from {
                ticks.push(tick);
            }
            tick *= 10.;
        }
        ticks
    } else {
        let step = nice_step(to / 4.);
        (0..=8)
            .map(|i| i as f64 * step)
            .filter(|t| *t <= to)
            .collect()
    };
    let bottom = TOP + rows.len() as f64 * ROW;
    for tick in ticks {
        let at = x(tick);
        let _ = write!(
            svg,
            r#"<line class="grid" x1="{at:.1}" x2="{at:.1}" y1="{TOP}" y2="{bottom}"/><text x="{at:.1}" y="{:.1}" text-anchor="middle">{}</text>"#,
            bottom + 15.,
            ms(tick)
        );
    }
    let _ = write!(
        svg,
        r#"<line class="axis" x1="{LABELS}" x2="{LABELS}" y1="{TOP}" y2="{bottom}"/>"#
    );
    for (i, row) in rows.iter().enumerate() {
        let y = TOP + i as f64 * ROW + ROW / 2.;
        let s = &row.summary;
        let tip = escape(&format!(
            "{}, {}: median {}, min {}, p90 {}, {} samples",
            row.label,
            row.series_name,
            ms(s.median),
            ms(s.min),
            ms(s.p90),
            s.n
        ));
        let color = COLORS[row.color % COLORS.len()];
        let _ = write!(
            svg,
            r#"<g><title>{tip}</title><rect class="hit" x="0" y="{:.1}" width="{WIDTH}" height="{ROW}"/><text class="label" x="{:.1}" y="{:.1}" text-anchor="end">{}</text><line x1="{:.1}" x2="{:.1}" y1="{y:.1}" y2="{y:.1}" stroke="{color}" stroke-width="2" stroke-linecap="round"/><circle cx="{:.1}" cy="{y:.1}" r="4.5" fill="{color}" stroke="var(--surface)" stroke-width="2"/></g>"#,
            y - ROW / 2.,
            LABELS - 8.,
            y + 4.,
            escape(&shorten(&row.label, 36)),
            x(s.min),
            x(s.p90).max(x(s.min) + 1.),
            x(s.median),
        );
    }
    svg.push_str("</svg>");
    (svg, log)
}

fn nice_step(rough: f64) -> f64 {
    if rough <= 0. {
        return 1.;
    }
    let magnitude = 10f64.powf(rough.log10().floor());
    [1., 2., 2.5, 5., 10.]
        .into_iter()
        .map(|m| m * magnitude)
        .find(|s| *s >= rough)
        .unwrap_or(magnitude * 10.)
}

fn shorten(text: &str, most: usize) -> String {
    if text.chars().count() <= most {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(most - 1).collect::<String>())
    }
}

/// The verdicts a bench gets: each variant of this build against the
/// baseline's, and each other variant against Monodrive's. Each holds the
/// wall-time verdict and, where both sides counted them, the instructions'.
fn verdicts(bench: &Bench) -> Vec<(String, Verdict, Option<Verdict>)> {
    let judge = |a: &Series, b: &Series| {
        let counted = (a.instructions(), b.instructions());
        let counted = (!counted.0.is_empty() && !counted.1.is_empty())
            .then(|| stats::compare_past(&counted.0, &counted.1, stats::INSTRUCTION_FLOOR));
        (stats::compare(&a.wall(), &b.wall()), counted)
    };
    let mut out = Vec::new();
    for series in bench.series.iter().filter(|s| s.build == "this") {
        if let Some(base) = bench
            .series
            .iter()
            .find(|s| s.build == "baseline" && s.variant == series.variant)
        {
            let label = if series.variant.is_empty() {
                "this against baseline".to_string()
            } else {
                format!("{}: this against baseline", series.variant)
            };
            let (wall, counted) = judge(series, base);
            out.push((label, wall, counted));
        }
    }
    if let Some(ours) = bench
        .series
        .iter()
        .find(|s| s.build == "this" && s.variant == "Monodrive")
    {
        for other in bench
            .series
            .iter()
            .filter(|s| s.build == "this" && s.variant != "Monodrive")
        {
            let (wall, counted) = judge(ours, other);
            out.push((
                format!("Monodrive against {}", other.variant),
                wall,
                counted,
            ));
        }
    }
    out
}

/// Each result whose instructions came out slower than the baseline's, as a
/// line: what `monodrive-bench gate` fails on.
pub fn slower_instructions(results: &Results) -> Vec<String> {
    results
        .suites
        .iter()
        .flat_map(|run| run.benches.iter().map(move |bench| (run, bench)))
        .flat_map(|(run, bench)| {
            verdicts(bench)
                .into_iter()
                .filter_map(move |(who, _, counted)| match counted {
                    Some(slower @ Verdict::Slower { .. })
                        if who.ends_with("this against baseline") =>
                    {
                        Some(format!(
                            "{}/{}: {}",
                            run.suite,
                            bench.bench,
                            verdict(slower).0
                        ))
                    }
                    _ => None,
                })
        })
        .collect()
}

fn group(
    page: &mut String,
    title: &str,
    benches: &[&Bench],
    builds: usize,
    saved: Option<&SuiteRun>,
) {
    let mut names: Vec<String> = Vec::new();
    let mut rows = Vec::new();
    for bench in benches {
        for series in &bench.series {
            let Some(summary) = summary(series) else {
                continue;
            };
            let name = series.name(builds);
            let color = names.iter().position(|n| *n == name).unwrap_or_else(|| {
                names.push(name.clone());
                names.len() - 1
            });
            // With more than one series, the label names it too, so a row
            // isn't told apart by its color alone.
            let label = if bench.series.len() > 1 {
                format!("{}, {name}", bench.title)
            } else {
                bench.title.clone()
            };
            rows.push(Row {
                label,
                series_name: name,
                color,
                summary,
                series,
                bench,
            });
        }
    }
    if rows.is_empty() {
        return;
    }
    let (svg, log) = chart(&rows);
    let cache = match benches[0].cache {
        Cache::Warm => "warm cache",
        Cache::Cold => "cold cache",
    };
    let scale = if log { "log scale" } else { "linear scale" };
    let _ = write!(
        page,
        r#"<div class="panel"><h3>{}</h3><p class="sub">Median dot, line from the fastest sample to p90, {cache}, {scale}. Hover a row for its values.</p>"#,
        escape(title)
    );
    if names.len() > 1 {
        page.push_str(r#"<div class="legend">"#);
        for (i, name) in names.iter().enumerate() {
            let _ = write!(
                page,
                r#"<span style="--c: {}">{}</span>"#,
                COLORS[i % COLORS.len()],
                escape(name)
            );
        }
        page.push_str("</div>");
    }
    let _ = write!(
        page,
        r#"<div class="figure"><div>{svg}</div><div class="scroll"><table><tr><th>What</th><th>Series</th><th>Median</th><th>Min</th><th>p90</th><th>n</th><th>Per item</th><th>CPU</th><th>Instructions</th>{}</tr>"#,
        if saved.is_some() {
            "<th>Saved run</th>"
        } else {
            ""
        }
    );
    for row in &rows {
        let s = &row.summary;
        let per_item = row
            .bench
            .items
            .filter(|n| *n > 0)
            .map_or_else(|| "–".to_string(), |n| ms(s.median / n as f64));
        let cpu = median_of(row.series.samples.iter().map(|s| s.cpu_ms))
            .map_or_else(|| "–".to_string(), ms);
        let instructions = median_of(
            row.series
                .samples
                .iter()
                .map(|s| s.instructions.map(|n| n as f64)),
        )
        .map_or_else(|| "–".to_string(), |n| format!("{:.1}M", n / 1e6));
        let checks = match row.series.checks.as_slice() {
            [one] => escape(one),
            many => format!(
                "<span class=\"slower\">rounds disagree: {}</span>",
                escape(&many.join("; "))
            ),
        };
        let _ = write!(
            page,
            r#"<tr><td>{}<small>{checks}</small></td><td class="series">{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{per_item}</td><td>{cpu}</td><td>{instructions}</td>"#,
            escape(&row.label),
            escape(&row.series_name),
            ms(s.median),
            ms(s.min),
            ms(s.p90),
            s.n,
        );
        if let Some(saved) = saved {
            let then = saved
                .benches
                .iter()
                .find(|b| b.bench == row.bench.bench)
                .and_then(|b| {
                    b.series
                        .iter()
                        .find(|x| x.build == "this" && x.variant == row.series.variant)
                })
                .and_then(summary)
                .map_or_else(|| "–".to_string(), |s| ms(s.median));
            let _ = write!(page, "<td>{then}</td>");
        }
        page.push_str("</tr>");
    }
    page.push_str("</table>");
    let judged: Vec<(String, String, Verdict, Option<Verdict>)> = benches
        .iter()
        .flat_map(|bench| {
            verdicts(bench)
                .into_iter()
                .map(|(who, wall, counted)| (bench.title.clone(), who, wall, counted))
        })
        .collect();
    if !judged.is_empty() {
        page.push_str(
            r#"<table><tr><th>What</th><th>Compared</th><th>Wall time</th><th>Instructions</th></tr>"#,
        );
        for (what, who, wall, counted) in judged {
            let (text, class) = verdict(wall);
            let (counted, counted_class) =
                counted.map_or(("not counted".to_string(), "flat"), verdict);
            let _ = write!(
                page,
                r#"<tr><td>{}</td><td class="series">{}</td><td class="{class}">{text}</td><td class="{counted_class}">{counted}</td></tr>"#,
                escape(&what),
                escape(&who)
            );
        }
        page.push_str("</table>");
    }
    page.push_str("</div></div></div>");
}

fn conditions(page: &mut String, run: &SuiteRun) {
    let c = &run.conditions;
    let _ = write!(
        page,
        r#"<details class="panel"><summary>Conditions</summary><dl>"#
    );
    for (name, value) in [
        (
            "Machine",
            format!("{} ({}), {} cores", c.hostname, c.model, c.cores),
        ),
        ("OS", format!("{} {}", c.os, c.macos_build)),
        ("Build", format!("{}, {}", c.commit, c.profile)),
        ("Volume", c.volume_format.clone()),
        ("Encryption", c.file_vault.clone()),
        ("Power", c.power.clone()),
        (
            "Thermal",
            c.thermal.lines().last().unwrap_or("").to_string(),
        ),
        (
            "Load",
            format!("{:.1} before, {:.1} after", c.load_before, c.load_after),
        ),
        (
            "Rounds",
            format!("{} measured after one warm-up", run.rounds),
        ),
    ] {
        let _ = write!(page, "<dt>{name}</dt><dd>{}</dd>", escape(&value));
    }
    page.push_str("</dl></details>");
}

fn budget_table(page: &mut String, judged: &[(Budget, Status)]) {
    page.push_str(r#"<h2 id="budgets">Budgets</h2><p class="sub">From benchmarks/budgets.toml, against the median of this build. A noisy suite isn't judged.</p><div class="panel scroll"><table><tr><th>What</th><th>Budget</th><th>Median</th><th>Status</th></tr>"#);
    for (budget, status) in judged {
        let (median, text, class) = match status {
            Status::Within { median_ms } => (ms(*median_ms), "within".to_string(), "within"),
            Status::Over { median_ms } => (ms(*median_ms), "over".to_string(), "over"),
            Status::NotJudged { median_ms } => (
                ms(*median_ms),
                "not judged, noisy run".to_string(),
                "missing",
            ),
            Status::Missing => (
                "–".to_string(),
                budget
                    .measured_by
                    .as_ref()
                    .map_or("not in this run".to_string(), |by| {
                        format!("measured by {by}")
                    }),
                "missing",
            ),
        };
        let why = budget.why.as_ref().map_or(String::new(), |why| {
            format!("<small>{}</small>", escape(why))
        });
        let _ = write!(
            page,
            r#"<tr><td>{}{why}</td><td>{}</td><td>{median}</td><td class="{class}">{}</td></tr>"#,
            escape(&budget.what),
            ms(budget.median_ms),
            escape(&text)
        );
    }
    page.push_str("</table></div>");
}

/// Platform results from two machines, side by side. They come from separate
/// runs on different hardware, so there's no verdict, only medians.
fn across_machines(page: &mut String, runs: &[&SuiteRun]) {
    let _ = write!(
        page,
        r#"<h2>{} across machines, different hardware</h2><p class="sub">Separate runs on different machines: medians side by side, with no verdict.</p><div class="panel scroll"><table><tr><th>What</th>"#,
        escape(&runs[0].suite)
    );
    for run in runs {
        let _ = write!(page, "<th>{}</th>", escape(&run.conditions.hostname));
    }
    page.push_str("</tr>");
    for bench in &runs[0].benches {
        let _ = write!(page, "<tr><td>{}</td>", escape(&bench.title));
        for run in runs {
            let median = run
                .benches
                .iter()
                .find(|b| b.bench == bench.bench)
                .and_then(|b| b.series.iter().find(|s| s.build == "this"))
                .and_then(summary)
                .map_or_else(|| "–".to_string(), |s| ms(s.median));
            let _ = write!(page, "<td>{median}</td>");
        }
        page.push_str("</tr>");
    }
    page.push_str("</table></div>");
}

pub fn render(results: &Results, judged: &[(Budget, Status)], saved: Option<&Results>) -> String {
    let mut page = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Monodrive benchmarks</title><style>{STYLE}</style></head><body><h1>Monodrive benchmarks</h1>"#
    );
    let builds: Vec<String> = results
        .builds
        .iter()
        .map(|b| format!("{} at {}", b.label, b.commit))
        .collect();
    let _ = write!(
        page,
        r#"<p class="lede">Run {}. Builds: {}. Each build's rounds alternate with the other's, and round 0 is thrown away.</p>"#,
        escape(&results.started),
        escape(&builds.join("; "))
    );
    let noisy: Vec<(String, Vec<String>)> = results
        .suites
        .iter()
        .map(|run| {
            (
                format!("{} on {}", run.suite, run.conditions.hostname),
                stats::noise(run),
            )
        })
        .filter(|(_, reasons)| !reasons.is_empty())
        .collect();
    if !noisy.is_empty() {
        page.push_str(r#"<div class="panel noise"><h3>Noisy run: don't publish these numbers, and budgets aren't judged</h3><ul>"#);
        for (suite, reasons) in noisy {
            let _ = write!(
                page,
                "<li>{}: {}</li>",
                escape(&suite),
                escape(&reasons.join("; "))
            );
        }
        page.push_str("</ul></div>");
    }
    budget_table(&mut page, judged);

    let builds = results.builds.len();
    let mut seen: BTreeMap<&str, Vec<&SuiteRun>> = BTreeMap::new();
    for run in &results.suites {
        seen.entry(&run.suite).or_default().push(run);
        let _ = write!(
            page,
            r#"<h2>{}</h2><p class="sub">On {}.</p>"#,
            escape(&run.suite),
            escape(&run.conditions.hostname)
        );
        for note in &run.notes {
            let _ = write!(page, r#"<p class="sub">{}</p>"#, escape(note));
        }
        conditions(&mut page, run);
        let saved_run = saved.and_then(|saved| {
            saved
                .suites
                .iter()
                .find(|s| s.suite == run.suite && s.conditions.hostname == run.conditions.hostname)
        });
        // Groups in the order they first appear, warm and cold apart.
        let mut groups: Vec<((String, Cache), Vec<&Bench>)> = Vec::new();
        for bench in &run.benches {
            let key = (bench.group.clone(), bench.cache);
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, benches)) => benches.push(bench),
                None => groups.push((key, vec![bench])),
            }
        }
        for ((title, _), benches) in &groups {
            group(&mut page, title, benches, builds, saved_run);
        }
    }
    for runs in seen.values() {
        let hosts: Vec<&str> = runs
            .iter()
            .map(|r| r.conditions.hostname.as_str())
            .collect();
        if runs.len() > 1 && hosts.iter().any(|h| *h != hosts[0]) {
            across_machines(&mut page, runs);
        }
    }
    page.push_str(r#"<p class="foot">Raw samples are in results.json beside this page. Verdicts come from a 95% bootstrap interval on the ratio of medians, from samples of the same run only; a comparison with fewer than 10 samples a side makes no claim. Instructions vary far less than wall time under load, so they show a change on a busy machine, but they miss time spent waiting; a difference in them counts only past 3%. CPU and instructions are medians per round, for the whole measuring process; instructions are counted only on macOS.</p></body></html>"#);
    page
}

/// `results.json` as Markdown for a pull request: a first line that says
/// whether anything needs a look, then each result of this build beside the
/// baseline's and its budget.
pub fn markdown(results: &Results, judged: &[(Budget, Status)]) -> String {
    let commit = |label: &str| {
        results
            .builds
            .iter()
            .find(|b| b.label == label)
            .map_or("", |b| b.commit.as_str())
    };
    let count = |n: usize| {
        if n == 1 {
            "1 result".to_string()
        } else {
            format!("{n} results")
        }
    };
    let (mut slower, mut over) = (0, 0);
    let mut rows = String::new();
    for run in &results.suites {
        for bench in &run.benches {
            for this in bench.series.iter().filter(|s| s.build == "this") {
                let Some(now) = summary(this) else {
                    continue;
                };
                // A bench the baseline doesn't have is new: no change, but
                // still a budget.
                let base = bench
                    .series
                    .iter()
                    .find(|s| s.build == "baseline" && s.variant == this.variant)
                    .filter(|s| !s.samples.is_empty());
                let (before, change) = match base {
                    Some(base) => {
                        let change = stats::compare(&this.wall(), &base.wall());
                        if matches!(change, Verdict::Slower { .. }) {
                            slower += 1;
                        }
                        (ms(stats::median(&base.wall())), verdict(change).0)
                    }
                    None => ("–".to_string(), "new".to_string()),
                };
                let key = format!("{}/{}", run.suite, bench.bench);
                let budget = judged
                    .iter()
                    .find(|(b, _)| this.variant.is_empty() && b.bench == key);
                let (limit, status) = match budget {
                    Some((b, Status::Within { .. })) => (ms(b.median_ms), "✅"),
                    Some((b, Status::Over { .. })) => {
                        over += 1;
                        (ms(b.median_ms), "❌")
                    }
                    Some((b, _)) => (ms(b.median_ms), "noisy"),
                    None => (String::new(), ""),
                };
                let title = if this.variant.is_empty() {
                    bench.title.clone()
                } else {
                    format!("{}, {}", bench.title, this.variant)
                };
                let _ = writeln!(
                    rows,
                    "| {} | {} | {} | {} | {} | {limit} | {status} |",
                    bench.group.replace('|', "\\|"),
                    title.replace('|', "\\|"),
                    before,
                    ms(now.median),
                    change,
                );
            }
        }
    }

    let mut out = String::from("## Benchmarks\n\n");
    if over > 0 {
        let _ = writeln!(out, "❌ {} over budget.\n", count(over));
    }
    if slower > 0 {
        let _ = writeln!(out, "⚠️ {} slower than the baseline.\n", count(slower));
    }
    let mut noisy = false;
    for run in &results.suites {
        let reasons = stats::noise(run);
        if !reasons.is_empty() {
            noisy = true;
            let _ = writeln!(
                out,
                "⚠️ The {} suite ran on a busy machine, so its budgets aren't judged: {}.\n",
                run.suite,
                reasons.join("; ")
            );
        }
    }
    if over == 0 && slower == 0 && !noisy {
        out.push_str("✅ Nothing ran slower than the baseline, and every budget holds.\n\n");
    }
    let _ = writeln!(
        out,
        "| Group | What | Baseline `{}` | This build `{}` | Change | Budget | |\n|---|---|--:|--:|---|--:|:-:|\n{rows}",
        commit("baseline"),
        commit("this"),
    );
    if let Some(run) = results.suites.first() {
        let c = &run.conditions;
        let _ = writeln!(
            out,
            "Medians of {} rounds a build, the builds taking turns on one {}, {} cores, macOS {}. A change is one only when a 95% bootstrap interval on the ratio of medians clears even. Budgets come from `benchmarks/budgets.toml` and are for an M3 Pro-class Mac.",
            run.rounds, c.model, c.cores, c.macos_build
        );
    }
    out
}

fn read(path: &Path) -> Result<Results, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn budgets_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/budgets.toml")
}

/// Prints `results_json` as Markdown, judged against the budgets.
pub fn print_markdown(results_json: &Path) -> Result<(), String> {
    let results = read(results_json)?;
    let judged = budgets::judge(&budgets::load(&budgets_path())?, &results);
    print!("{}", markdown(&results, &judged));
    Ok(())
}

/// Writes `report.html` beside `results_json` and returns its path.
pub fn write(results_json: &Path, saved: Option<&Path>) -> Result<PathBuf, String> {
    let results = read(results_json)?;
    let saved = saved.map(read).transpose()?;
    let judged = budgets::judge(&budgets::load(&budgets_path())?, &results);
    let out = results_json.with_file_name("report.html");
    std::fs::write(&out, render(&results, &judged, saved.as_ref()))
        .map_err(|e| format!("{}: {e}", out.display()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_clear_difference_gets_no_multiplier() {
        let (text, _) = verdict(Verdict::NoClearDifference);
        assert!(!text.contains('×'));
        let (text, class) = verdict(Verdict::Slower {
            low: 1.5,
            high: 2.25,
        });
        assert_eq!((text.as_str(), class), ("slower, 1.50× to 2.25×", "slower"));
    }

    #[test]
    fn the_gate_names_what_takes_more_instructions_and_nothing_else() {
        use crate::format::{Bench, Series, SuiteRun};
        use crate::measure::Sample;
        let series = |build: &str, instructions: u64| Series {
            build: build.into(),
            variant: String::new(),
            warmup: None,
            samples: (0..12)
                .map(|i| Sample {
                    wall_ms: 1.,
                    cpu_ms: None,
                    instructions: Some(instructions + i),
                    cycles: None,
                })
                .collect(),
            checks: vec![],
        };
        let results = |this: u64| Results {
            version: crate::format::RESULTS,
            started: String::new(),
            builds: vec![],
            suites: vec![SuiteRun {
                suite: "core".into(),
                conditions: crate::conditions::tests::quiet(),
                rounds: 12,
                notes: vec![],
                benches: vec![Bench {
                    bench: "list_dir/10000".into(),
                    title: String::new(),
                    group: String::new(),
                    items: None,
                    cache: Cache::Warm,
                    series: vec![series("this", this), series("baseline", 1_000_000)],
                }],
            }],
        };
        assert_eq!(
            slower_instructions(&results(2_000_000)),
            ["core/list_dir/10000: slower, 2.00× to 2.00×"]
        );
        assert!(slower_instructions(&results(1_010_000)).is_empty());
        assert!(slower_instructions(&results(500_000)).is_empty());
    }

    #[test]
    fn markdown_leads_with_what_got_slower_or_went_past_its_budget() {
        use crate::measure::Sample;
        let series = |build: &str, wall_ms: f64| Series {
            build: build.into(),
            variant: String::new(),
            warmup: None,
            samples: (0..12)
                .map(|i| Sample {
                    wall_ms: wall_ms + i as f64 * 0.01,
                    cpu_ms: None,
                    instructions: None,
                    cycles: None,
                })
                .collect(),
            checks: vec![],
        };
        let results = |this: f64| Results {
            version: crate::format::RESULTS,
            started: String::new(),
            builds: vec![],
            suites: vec![SuiteRun {
                suite: "core".into(),
                conditions: crate::conditions::tests::quiet(),
                rounds: 12,
                notes: vec![],
                benches: vec![Bench {
                    bench: "list_dir/10000".into(),
                    title: "List 10,000".into(),
                    group: "List a folder".into(),
                    items: None,
                    cache: Cache::Warm,
                    series: vec![series("this", this), series("baseline", 10.)],
                }],
            }],
        };
        let budgets: budgets::Budgets = toml::from_str(
            r#"
            [[budget]]
            bench = "core/list_dir/10000"
            what = "List 10,000"
            median_ms = 20
            "#,
        )
        .unwrap();
        let comment = |this: f64| {
            let results = results(this);
            markdown(&results, &budgets::judge(&budgets, &results))
        };

        let same = comment(10.);
        assert!(same.contains("✅ Nothing ran slower"), "{same}");
        assert!(
            same.contains(
                "| List a folder | List 10,000 | 10.1 ms | 10.1 ms | no clear difference | 20.0 ms | ✅ |"
            ),
            "{same}"
        );

        let worse = comment(30.);
        assert!(worse.contains("❌ 1 result over budget."), "{worse}");
        assert!(
            worse.contains("⚠️ 1 result slower than the baseline."),
            "{worse}"
        );
        assert!(!worse.contains('✅'), "{worse}");

        let mut added = results(30.);
        added.suites[0].benches[0]
            .series
            .retain(|s| s.build == "this");
        let added = markdown(&added, &budgets::judge(&budgets, &added));
        assert!(added.contains("❌ 1 result over budget."), "{added}");
        assert!(
            added.contains("| List a folder | List 10,000 | – | 30.1 ms | new | 20.0 ms | ❌ |"),
            "{added}"
        );
    }
}
