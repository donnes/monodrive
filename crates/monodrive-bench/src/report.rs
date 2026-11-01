//! `results.json` as one HTML page, with no scripts and nothing fetched. It
//! reads only that file, so it can't show a number the run didn't produce.

use crate::search::{Accuracy, CaseResult, Results, Tool, ToolResult};
use crate::stats::{self, Verdict};
use std::fmt::Write;
use std::path::Path;

const STYLE: &str = r#"
:root {
  --bg: #09090b; --surface: #0c0c0e; --surface-2: #121214; --rule: #27272a; --grid: #18181b;
  --fg: #fafafa; --fg-2: #e4e4e7; --muted: #d4d4d8; --body: #a1a1aa; --dim: #71717a; --faint: #52525b;
  --accent: #86efac; --bar: #3f3f46; --bad: #fca5a5;
  --font: "Inter", -apple-system, BlinkMacSystemFont, system-ui, sans-serif;
  --mono: "JetBrains Mono", "Geist Mono", ui-monospace, "SF Mono", Menlo, monospace;
  --pad: clamp(20px, 5vw, 96px);
  --radius: 6px;
}
html { background: var(--bg); color: var(--fg); font-family: var(--font); }
body { margin: 0; padding: var(--pad); }
.eyebrow { font: 12px var(--mono); text-transform: uppercase; letter-spacing: .2em; color: var(--dim); margin: 0 0 16px; }
h1 { font-weight: 700; line-height: 1.05; letter-spacing: -.035em; font-size: clamp(32px, 5.5vw, 64px); margin: 0 0 16px; overflow-wrap: anywhere; }
h1 em { font-style: normal; color: var(--accent); }
h1 span { display: block; color: var(--muted); font-size: .6em; margin-top: .3em; letter-spacing: -.02em; }
.lede { font-size: clamp(15px, 1.8vw, 20px); line-height: 1.45; color: var(--body); margin: 0 0 28px; max-width: 70ch; }
.panel { background: var(--surface); border: 1px solid var(--rule); border-radius: var(--radius); margin: 0 0 20px; overflow: hidden; }
.panel > header { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 4px 16px; background: var(--surface-2); border-bottom: 1px solid var(--rule); padding: 10px 16px; }
.panel h2 { font: 12px var(--mono); text-transform: uppercase; letter-spacing: .15em; color: var(--fg-2); margin: 0; }
.panel header small { font: 12px var(--mono); color: var(--dim); }
.noise { border-color: var(--bad); }
.noise ul { margin: 0; padding: 12px 16px 12px 32px; color: var(--bad); font: 13px/1.6 var(--mono); }
.scroll { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font: 13px var(--mono); font-variant-numeric: tabular-nums; }
th { color: var(--dim); font-weight: 400; text-align: right; padding: 10px 8px; border-bottom: 1px solid var(--rule); white-space: nowrap; }
td { color: var(--fg-2); text-align: right; padding: 10px 8px; border-bottom: 1px solid var(--grid); white-space: nowrap; }
tr:last-child td { border-bottom: 0; }
th:first-child, td:first-child { text-align: left; }
td:first-child { font-family: var(--font); }
td small { display: block; color: var(--dim); font-family: var(--mono); font-size: 11px; }
.win { color: var(--accent); } .lose { color: var(--bad); } .flat { color: var(--dim); }
.rows { padding: 14px 16px 16px; }
.row { margin: 0 0 12px; }
.row:last-child { margin: 0; }
.row p { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 2px 12px; margin: 0 0 4px; font-size: 13px; color: var(--fg-2); }
.row p span { font: 12px var(--mono); color: var(--dim); font-variant-numeric: tabular-nums; }
.row p b { color: var(--fg-2); font-weight: 400; }
.track { height: 8px; }
.fill { height: 8px; min-width: 2px; background: var(--bar); border-radius: 0 4px 4px 0; }
.fill.us { background: var(--accent); }
.fill.them { background: var(--fg); }
dl { display: grid; grid-template-columns: minmax(110px, max-content) 1fr; gap: 6px 16px; margin: 0; padding: 14px 16px; font-size: 13px; }
dt { color: var(--dim); font-family: var(--mono); font-size: 12px; }
dd { margin: 0; color: var(--fg-2); overflow-wrap: anywhere; }
.foot { font: 12px/1.6 var(--mono); color: var(--faint); margin: 20px 0 0; }
.foot b { color: var(--body); font-weight: 400; }
"#;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn tool(case: &CaseResult, wanted: Tool) -> Option<&ToolResult> {
    case.tools.iter().find(|tool| tool.tool == wanted)
}

fn thousands(n: usize) -> String {
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

/// A median at the precision its size deserves.
fn ms(value: f64) -> String {
    if value < 10. {
        format!("{value:.2} ms")
    } else {
        format!("{value:.1} ms")
    }
}

fn times(value: f64) -> String {
    if value < 10. {
        format!("{value:.1}")
    } else {
        format!("{value:.0}")
    }
}

fn found(accuracy: Accuracy) -> String {
    if accuracy.reachable == 0 {
        "none expected".to_string()
    } else {
        format!("{} of {}", accuracy.found, accuracy.reachable)
    }
}

/// How many queries fsearch won and the range of those wins. A query with no
/// clear difference adds no multiplier.
fn win_range<'a>(cases: impl Iterator<Item = &'a CaseResult>) -> Option<(usize, f64, f64)> {
    let wins: Vec<(f64, f64)> = cases
        .filter_map(|case| match case.fsearch_against_spotlight {
            Verdict::Faster { low, high } => Some((low, high)),
            _ => None,
        })
        .collect();
    if wins.is_empty() {
        return None;
    }
    let low = wins.iter().map(|w| w.0).fold(f64::INFINITY, f64::min);
    let high = wins.iter().map(|w| w.1).fold(0., f64::max);
    Some((wins.len(), low, high))
}

/// The page's answer: how often fsearch won, by how much, and what it gave up.
fn headline(results: &Results) -> String {
    let total = results.cases.len();
    let speed = match win_range(results.cases.iter()) {
        Some((wins, low, high)) => format!(
            "fsearch beat Spotlight on <em>{wins} of {total}</em> queries, by {} to {} times",
            times(low),
            times(high),
        ),
        None => format!("fsearch beat Spotlight on <em>0 of {total}</em> queries"),
    };
    let fewer = results
        .cases
        .iter()
        .filter(
            |case| match (tool(case, Tool::FsearchAll), tool(case, Tool::SpotlightApi)) {
                (Some(ours), Some(theirs)) => ours.accuracy.found < theirs.accuracy.found,
                _ => false,
            },
        )
        .count();
    format!("{speed}<span>It found fewer of the expected names on {fewer} of {total}.</span>")
}

fn verdict_cell(verdict: Verdict) -> String {
    match verdict {
        Verdict::Faster { low, high } => format!(
            r#"<td class="win">{} to {}× faster</td>"#,
            times(low),
            times(high)
        ),
        Verdict::Slower { low, high } => format!(
            r#"<td class="lose">{} to {}× slower</td>"#,
            times(low),
            times(high)
        ),
        Verdict::NoClearDifference => r#"<td class="flat">no clear difference</td>"#.to_string(),
        Verdict::TooFewSamples => r#"<td class="flat">too few samples</td>"#.to_string(),
    }
}

fn summary(page: &mut String, results: &Results) {
    page.push_str(concat!(
        r#"<section class="panel"><header><h2>fsearch against Spotlight</h2><small>lower is better, MDQuery in process</small></header>"#,
        r#"<div class="scroll"><table><thead><tr><th>Query</th><th>fsearch</th><th>Spotlight</th><th>95% range</th><th>fsearch found</th><th>Spotlight</th></tr></thead><tbody>"#,
    ));
    for case in &results.cases {
        let (Some(ours), Some(theirs)) =
            (tool(case, Tool::FsearchAll), tool(case, Tool::SpotlightApi))
        else {
            continue;
        };
        let class = |a: Accuracy, b: Accuracy| {
            if a.found < b.found {
                " class=\"lose\""
            } else {
                ""
            }
        };
        let _ = write!(
            page,
            "<tr><td>{}<small>{}</small></td><td>{}</td><td>{}</td>{}<td{}>{}</td><td{}>{}</td></tr>",
            escape(&case.label),
            escape(&case.text),
            ms(stats::median(&ours.samples_ms)),
            ms(stats::median(&theirs.samples_ms)),
            verdict_cell(case.fsearch_against_spotlight),
            class(ours.accuracy, theirs.accuracy),
            found(ours.accuracy),
            class(theirs.accuracy, ours.accuracy),
            found(theirs.accuracy),
        );
    }
    page.push_str("</tbody></table></div></section>");
}

fn case_panel(page: &mut String, case: &CaseResult) {
    let longest = case
        .tools
        .iter()
        .map(|tool| stats::median(&tool.samples_ms))
        .fold(0., f64::max);
    let _ = write!(
        page,
        r#"<section class="panel"><header><h2>{}</h2><small>{}, linear scale from zero</small></header><div class="rows">"#,
        escape(&case.label),
        escape(&case.text),
    );
    for tool in &case.tools {
        let samples = &tool.samples_ms;
        let median = stats::median(samples);
        let min = samples.iter().copied().fold(f64::INFINITY, f64::min);
        let p90 = stats::percentile(samples, 0.9);
        let width = if longest > 0. {
            median / longest * 100.
        } else {
            0.
        };
        let mark = match tool.tool {
            Tool::FsearchWindow | Tool::FsearchAll => " us",
            Tool::SpotlightApi => " them",
            _ => "",
        };
        let accuracy = tool.accuracy;
        let missed = if accuracy.found < accuracy.reachable {
            " class=\"lose\""
        } else {
            ""
        };
        let changed = if tool.stable {
            ""
        } else {
            ", results changed between rounds"
        };
        let _ = write!(
            page,
            concat!(
                r#"<div class="row"><p>{name}<span><b>{median}</b>, min {min}, p90 {p90}, n={n}, "#,
                r#"found <i{missed} style="font-style:normal">{found}</i>, {extra} extra{changed}</span></p>"#,
                r#"<div class="track"><div class="fill{mark}" style="width:{width:.2}%" title="{name}: {median}"></div></div></div>"#
            ),
            name = escape(&tool.name),
            median = ms(median),
            min = ms(min),
            p90 = ms(p90),
            n = samples.len(),
            missed = missed,
            found = found(accuracy),
            extra = accuracy.extra,
            changed = changed,
            mark = mark,
            width = width,
        );
    }
    page.push_str("</div></section>");
}

fn conditions(page: &mut String, results: &Results) {
    let c = &results.conditions;
    let last_line = |text: &str| text.lines().last().unwrap_or("unknown").trim().to_string();
    let rows = [
        ("Commit", c.commit.clone()),
        ("Machine", format!("{}, {} cores", c.model, c.cores)),
        ("macOS build", c.macos_build.clone()),
        ("Volume", c.volume_format.clone()),
        ("FileVault", c.file_vault.clone()),
        ("Power", c.power.clone()),
        ("Spotlight", last_line(&c.spotlight)),
        (
            "Load",
            format!("{:.1} before, {:.1} after", c.load_before, c.load_after),
        ),
        (
            "Corpus",
            format!(
                "{} entries, seed {}, {} in Spotlight",
                thousands(results.entries),
                results.seed,
                thousands(results.spotlight_indexed)
            ),
        ),
        (
            "fsearch index",
            format!(
                "built in {}, {:.1} MB in memory",
                ms(results.index_build_ms),
                results.index_bytes as f64 / 1e6
            ),
        ),
        (
            "Process start",
            format!(
                "{} median, included in mdfind, find and fd",
                ms(stats::median(&results.process_start_ms))
            ),
        ),
    ];
    page.push_str(r#"<section class="panel"><header><h2>Conditions</h2></header><dl>"#);
    for (name, value) in rows {
        let _ = write!(page, "<dt>{name}</dt><dd>{}</dd>", escape(&value));
    }
    page.push_str("</dl></section>");
}

/// Two times and no ratio: Spotlight also reads contents and metadata, as a
/// background service, and fsearch lists names, so one doesn't divide the other.
fn indexing(page: &mut String, results: &Results) {
    let rebuilt = if results.index_rebuild_ms.is_empty() {
        "not timed".to_string()
    } else {
        format!(
            "{} median, n={}",
            ms(stats::median(&results.index_rebuild_ms)),
            results.index_rebuild_ms.len()
        )
    };
    let spotlight = results
        .spotlight_index_seconds
        .map_or("not timed".to_string(), |seconds| format!("{seconds:.0} s"));
    let _ = write!(
        page,
        concat!(
            r#"<section class="panel"><header><h2>Indexing {entries} entries</h2><small>different work, so no ratio</small></header><dl>"#,
            "<dt>fsearch, first</dt><dd>{first}, right after remounting the volume. The disk image's blocks may still be in memory.</dd>",
            "<dt>fsearch, again</dt><dd>{rebuilt}. Names, sizes and dates only.</dd>",
            "<dt>Spotlight</dt><dd>{spotlight}, from the first file written until every entry was searchable. It also reads contents and metadata, as a background service.</dd>",
            "</dl></section>",
        ),
        entries = thousands(results.entries),
        first = ms(results.index_build_ms),
        rebuilt = rebuilt,
        spotlight = spotlight,
    );
}

pub fn render(results: &Results) -> String {
    let mut page = format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Monodrive name search benchmark</title><style>{STYLE}</style></head><body><p class="eyebrow">// Monodrive benchmark, name search</p><h1>{}</h1><p class="lede">Each tool searched the same {} generated files. The answer key is every name holding the query's words, ignoring case and accents.</p>"#,
        headline(results),
        thousands(results.entries),
    );
    if !results.noise.is_empty() {
        page.push_str(r#"<section class="panel noise"><header><h2>Noisy run, don't publish</h2></header><ul>"#);
        for reason in &results.noise {
            let _ = write!(page, "<li>{}</li>", escape(reason));
        }
        page.push_str("</ul></section>");
    }
    summary(&mut page, results);
    for case in &results.cases {
        case_panel(&mut page, case);
    }
    indexing(&mut page, results);
    conditions(&mut page, results);
    let rounds = results
        .cases
        .first()
        .and_then(|case| case.tools.first())
        .map_or(0, |tool| tool.samples_ms.len());
    let _ = write!(
        page,
        r#"<p class="foot">// <b>{rounds}</b> rounds per query, tools in turns with the order reversed each round, and a first round thrown away. fsearch and Spotlight's MDQuery run in process and return paths. mdfind, find and fd are separate processes. Intervals are a 95% bootstrap on the ratio of medians. Extra counts results outside the key, and fsearch matches fuzzily, so it isn't an error count. A typo query's key is the word meant. Spotlight's 0 of 5 on the Japanese query is unexplained: it found those names on a small volume, so the post image leaves that query out. This measures one volume, not a whole disk.</p></body></html>"#
    );
    page
}

const POST_STYLE: &str = r#"
html, body { margin: 0; width: 1200px; height: 675px; overflow: hidden; background: #09090b; color: #fafafa; font-family: "Inter", -apple-system, BlinkMacSystemFont, system-ui, sans-serif; }
body { box-sizing: border-box; padding: 30px 48px 22px; display: flex; flex-direction: column; }
.brand { font-size: 19px; font-weight: 600; color: #a1a1aa; margin: 0 0 6px; }
h1 { font-size: 36px; line-height: 1.12; letter-spacing: -.025em; font-weight: 700; margin: 0 0 14px; }
h1 em { font-style: normal; color: #86efac; white-space: nowrap; }
.head, .case { display: grid; grid-template-columns: 216px 1fr 150px; column-gap: 20px; align-items: center; }
.head { font-size: 16px; color: #a1a1aa; padding-bottom: 6px; border-bottom: 1px solid #27272a; }
.head span:last-child, .found { text-align: right; }
.key { display: flex; gap: 24px; }
.key i { display: inline-block; width: 12px; height: 12px; border-radius: 2px; margin-right: 8px; }
.cases { flex: 1; display: flex; flex-direction: column; }
.case { flex: 1; border-bottom: 1px solid #18181b; }
.case:last-child { border-bottom: 0; }
.name { font-size: 17px; line-height: 1.12; color: #e4e4e7; }
.name small { display: block; font: 14px ui-monospace, "SF Mono", Menlo, monospace; color: #a1a1aa; }
.line { display: flex; align-items: center; height: 20px; font-size: 16px; font-variant-numeric: tabular-nums; }
.line i { display: block; height: 10px; min-width: 2px; margin-right: 8px; border-radius: 0 3px 3px 0; }
.us { background: #86efac; } .them { background: #fafafa; }
.found { font-size: 16px; line-height: 20px; font-variant-numeric: tabular-nums; color: #e4e4e7; }
.found span { display: block; }
.found .lose { color: #fca5a5; }
.none { color: #a1a1aa; }
.credit { font-size: 16px; color: #a1a1aa; margin: 6px 0 0; padding-top: 10px; border-top: 1px solid #27272a; }
.credit b { font-weight: 600; color: #fafafa; }
"#;

/// In the report, but not on the image. Spotlight found 0 of 5 of these names
/// in the 2026-11-01 run, and all of them on a small volume minutes later, so
/// that result is unexplained and shouldn't be published as Spotlight's.
const NOT_ON_POST: &[&str] = &["Japanese"];

/// The room each bar leaves for the time printed after it.
const VALUE_SLOT: &str = "96px";

fn post_case(page: &mut String, case: &CaseResult, longest: f64) {
    let (Some(ours), Some(theirs)) = (tool(case, Tool::FsearchAll), tool(case, Tool::SpotlightApi))
    else {
        return;
    };
    let line = |class: &str, samples: &[f64]| {
        let median = stats::median(samples);
        let share = if longest > 0. { median / longest } else { 0. };
        format!(
            r#"<div class="line"><i class="{class}" style="width:calc((100% - {VALUE_SLOT}) * {share:.4})"></i>{}</div>"#,
            ms(median)
        )
    };
    let found_cell = if ours.accuracy.reachable == 0 && theirs.accuracy.reachable == 0 {
        r#"<span class="none">none expected</span>"#.to_string()
    } else {
        let span = |a: Accuracy, b: Accuracy| {
            let class = if a.found < b.found {
                " class=\"lose\""
            } else {
                ""
            };
            format!("<span{class}>{}</span>", found(a))
        };
        span(ours.accuracy, theirs.accuracy) + &span(theirs.accuracy, ours.accuracy)
    };
    let _ = write!(
        page,
        r#"<div class="case"><div class="name">{}<small>{}</small></div><div>{}{}</div><div class="found">{found_cell}</div></div>"#,
        escape(&case.label),
        escape(&case.text),
        line("us", &ours.samples_ms),
        line("them", &theirs.samples_ms),
    );
}

/// A 1200 by 675 page for a post on X, which `scripts/html-to-png.swift` turns
/// into an image. Like the report, it reads only `results.json` and keeps
/// losses. A run's conditions and noise stay in the report, not on the image.
pub fn render_post(results: &Results) -> String {
    let cases: Vec<&CaseResult> = results
        .cases
        .iter()
        .filter(|case| !NOT_ON_POST.contains(&case.label.as_str()))
        .collect();
    let total = cases.len();
    let title = match win_range(cases.iter().copied()) {
        Some((wins, low, high)) => format!(
            "fsearch beat Spotlight on <em>{wins} of {total}</em> name searches, by {} to {} times",
            times(low),
            times(high),
        ),
        None => format!("fsearch beat Spotlight on <em>0 of {total}</em> name searches"),
    };
    let longest = cases
        .iter()
        .flat_map(|case| [tool(case, Tool::FsearchAll), tool(case, Tool::SpotlightApi)])
        .flatten()
        .map(|tool| stats::median(&tool.samples_ms))
        .fold(0., f64::max);
    let mut page = format!(
        concat!(
            r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Monodrive name search</title><style>{style}</style></head><body>"#,
            r#"<p class="brand">fsearch, the search engine behind Monodrive</p><h1>{title}</h1>"#,
            r#"<div class="head"><span>Query</span><span class="key"><span><i class="us"></i>fsearch</span><span><i class="them"></i>Spotlight (MDQuery)</span><span>median time, shorter is faster</span></span><span>Names found</span></div><div class="cases">"#,
        ),
        style = POST_STYLE,
        title = title,
    );
    for case in cases {
        post_case(&mut page, case, longest);
    }
    page.push_str(
        r#"</div><p class="credit"><b>fsearch</b> by Noah Dunnagan, github.com/noahdunnagan/fsearch</p></body></html>"#,
    );
    page
}

/// Writes `report.html` and `post.html` beside `results.json`, and returns the report's path.
pub fn write(results_json: &Path) -> Result<std::path::PathBuf, String> {
    let bytes = std::fs::read(results_json)
        .map_err(|error| format!("{}: {error}", results_json.display()))?;
    let results: Results = serde_json::from_slice(&bytes)
        .map_err(|error| format!("{}: {error}", results_json.display()))?;
    let out = results_json.with_file_name("report.html");
    std::fs::write(&out, render(&results))
        .map_err(|error| format!("{}: {error}", out.display()))?;
    let post = results_json.with_file_name("post.html");
    std::fs::write(&post, render_post(&results))
        .map_err(|error| format!("{}: {error}", post.display()))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conditions::Conditions;

    fn sampled(tool: Tool, found: usize) -> ToolResult {
        ToolResult {
            tool,
            name: format!("{}", tool as u8),
            warmup_ms: 1.,
            samples_ms: vec![1.; 3],
            accuracy: Accuracy {
                hits: found,
                expected: 6,
                reachable: 6,
                found,
                missed: 6 - found,
                extra: 0,
            },
            stable: true,
        }
    }

    fn results(verdict: Verdict, noise: &[&str], ours_found: usize) -> Results {
        Results {
            version: 1,
            seed: 1,
            conditions: Conditions {
                commit: String::new(),
                model: String::new(),
                cores: 11,
                macos_build: String::new(),
                volume_format: String::new(),
                file_vault: String::new(),
                power: String::new(),
                thermal: String::new(),
                spotlight: String::new(),
                load_before: 1.,
                load_after: 1.,
            },
            noise: noise.iter().map(ToString::to_string).collect(),
            entries: 10,
            spotlight_indexed: 10,
            index_build_ms: 1.,
            index_rebuild_ms: vec![1.],
            spotlight_index_seconds: Some(821.),
            index_bytes: 1,
            process_start_ms: vec![2.],
            cases: vec![CaseResult {
                label: "<word>".to_string(),
                text: "w".to_string(),
                tools: vec![
                    sampled(Tool::FsearchAll, ours_found),
                    sampled(Tool::SpotlightApi, 6),
                ],
                fsearch_against_spotlight: verdict,
            }],
        }
    }

    #[test]
    fn no_clear_difference_prints_no_multiplier_anywhere() {
        let page = render(&results(Verdict::NoClearDifference, &[], 6));
        assert!(page.contains("no clear difference"));
        assert!(page.contains("821 s"));
        assert!(page.contains("<em>0 of 1</em>"));
        assert!(!page.contains(" times") && !page.contains('×'));
    }

    #[test]
    fn a_loss_stays_on_the_page() {
        let page = render(&results(Verdict::Slower { low: 2., high: 3. }, &[], 3));
        assert!(page.contains("2.0 to 3.0× slower"));
        assert!(page.contains("fewer of the expected names on 1 of 1"));
    }

    #[test]
    fn a_noisy_run_says_so_and_a_quiet_one_doesnt() {
        let faster = Verdict::Faster { low: 2., high: 3. };
        let noisy = render(&results(faster, &["on battery"], 6));
        assert!(noisy.contains("don't publish") && noisy.contains("on battery"));
        assert!(!render(&results(faster, &[], 6)).contains("don't publish"));
    }

    #[test]
    fn the_post_keeps_losses_and_invents_no_multiplier() {
        let post = render_post(&results(Verdict::NoClearDifference, &[], 3));
        assert!(post.contains(r#"<span class="lose">3 of 6</span>"#));
        assert!(post.contains("<em>0 of 1</em>") && !post.contains(" times"));
    }

    #[test]
    fn names_are_escaped() {
        let page = render(&results(Verdict::NoClearDifference, &[], 6));
        assert!(page.contains("&lt;word&gt;") && !page.contains("<word>"));
    }
}
