//! Frame draw times while scrolling, dragging and sliding the sidebar over
//! 10,000 entries: the app's `scrolling` benchmark, run for about a second a
//! case through GPUI's headless Metal renderer. Each case's sample is the p99
//! of the draws its GPUI report saw that round.
//!
//! `scripts/bench` builds the benchmark and links it beside this tool as
//! `monodrive-frames`. It makes and deletes its own folders.

use crate::format::{Cache, Measured};
use crate::measure::Sample;
use std::path::Path;
use std::process::{Command, Stdio};

const BINARY: &str = "monodrive-frames";
const REPORT: &str = "GPUI bench report (all observed iterations): ";

/// The benchmark's cases, and what each is called in a chart.
const CASES: [(&str, &str); 7] = [
    ("scroll_list", "Scroll the list"),
    (
        "scroll_list_with_inspector",
        "Scroll the list beside the inspector",
    ),
    ("scroll_grid", "Scroll the grid"),
    ("scroll_columns", "Scroll column view"),
    ("drag_over_list", "Drag a file over the list"),
    ("slide_sidebar_list", "Slide the sidebar over the list"),
    ("slide_sidebar_grid", "Slide the sidebar over the grid"),
];

/// Each case's p99 draw time in milliseconds, from the benchmark's stderr.
fn parse(stderr: &str) -> Vec<(String, f64)> {
    let mut found = Vec::new();
    let mut case = None;
    let mut in_draw = false;
    for line in stderr.lines() {
        if let Some(name) = line.strip_prefix(REPORT) {
            case = Some(name.trim().to_string());
            in_draw = false;
        } else if line.starts_with("  ") && !line.starts_with("    ") {
            in_draw = line.trim() == "window draw:";
        } else if let (true, Some(p99)) = (in_draw, line.trim().strip_prefix("p99: ")) {
            let ms = p99.strip_suffix("ms").and_then(|ms| ms.parse().ok());
            if let (Some(case), Some(ms)) = (case.take(), ms) {
                found.push((case, ms));
            }
        }
    }
    found
}

pub fn round(fixture: &Path) -> Result<Vec<Measured>, String> {
    let binary = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name(BINARY);
    if !binary.exists() {
        return Err(format!(
            "{}: scripts/bench links it after cargo bench -p monodrive --bench scrolling --no-run",
            binary.display()
        ));
    }
    let out = Command::new(&binary)
        .args(["--bench", "--profile-time", "1"])
        // Wall time only: the report's p99 is what's kept.
        .env("BENCH_MEASUREMENT", "wall-time")
        .env("CRITERION_HOME", fixture.join("criterion"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{}: {e}", binary.display()))?;
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Err(format!("{}: {}\n{stderr}", binary.display(), out.status));
    }
    let found = parse(&stderr);
    CASES
        .iter()
        .map(|&(case, title)| {
            let mut reported = found.iter().filter(|(name, _)| name == case);
            let (Some(&(_, p99)), None) = (reported.next(), reported.next()) else {
                return Err(format!(
                    "{}: no single draw p99 for {case}\n{stderr}",
                    binary.display()
                ));
            };
            Ok(Measured {
                title: format!("{title}, p99 frame"),
                bench: case.to_string(),
                group: "Frames".into(),
                variant: String::new(),
                items: None,
                cache: Cache::Warm,
                sample: Sample {
                    wall_ms: p99,
                    cpu_ms: None,
                    instructions: None,
                    cycles: None,
                },
                check: "10000 entries".into(),
                peak_bytes: None,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn each_case_keeps_its_draw_p99_and_not_the_others() {
        let stderr = "\
GPUI bench report (all observed iterations): scroll_list
  note: includes Criterion warmup/calibration
  window dirty-to-draw:
    samples: 480
    p99: 9.000ms
  window draw:
    samples: 480
    p50: 1.100ms
    p99: 1.612ms
    max: 3.000ms
  foreground executor work (task polls, actions, input dispatch):
    p99: 7.000ms
GPUI bench report (all observed iterations): drag_over_list
  window draw:
    p99: 2.500ms
";
        assert_eq!(
            super::parse(stderr),
            [
                ("scroll_list".to_string(), 1.612),
                ("drag_over_list".to_string(), 2.5)
            ]
        );
    }
}
