//! Launching the app's window on a folder, read from the launch line a build
//! with the `profile` feature prints: to drawing the folder's contents, and to
//! the frame signal by which it's on the display.
//!
//! Each round starts the app in a clone of a home holding the generated
//! index, so it loads that and crawls nothing, with `MONODRIVE_UNATTENDED`, so
//! it never activates and its window opens past the edge of the screen. A real
//! launch activates the app too. The app is killed by the PID captured when it
//! started.

use crate::format::{Cache, Measured};
use crate::measure::Sample;
use crate::suites::{ShortPath, delete_fixture, mcp, search};
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const OPENED: usize = 100;
/// Far past any launch, so only a build without `profile` or a hung app waits
/// this long.
const PATIENCE: Duration = Duration::from_secs(30);

fn io(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

pub fn prepare(fixture: &Path, seed: u64) -> Result<(), String> {
    search::prepare(fixture, seed)?;
    mcp::home_with_index(fixture, &fixture.join("home"))?;
    // A folder about the size of a home or Documents, in kinds that draw
    // different icons.
    let opened = fixture.join("opened");
    std::fs::create_dir(&opened).map_err(io(&opened))?;
    const EXTS: [&str; 6] = ["jpg", "pdf", "png", "md", "txt", "rs"];
    for i in 0..OPENED {
        if i % 10 == 0 {
            let path = opened.join(format!("folder {i}"));
            std::fs::create_dir(&path).map_err(io(&path))?;
        } else {
            let path = opened.join(format!("file {i}.{}", EXTS[i % EXTS.len()]));
            std::fs::write(&path, i.to_le_bytes()).map_err(io(&path))?;
        }
    }
    Ok(())
}

/// The launch line's times, in milliseconds: the folder's contents drawn, and
/// on the display.
fn parse(line: &str) -> Option<(f64, f64)> {
    let after = |label: &str| -> Option<f64> {
        let rest = &line[line.find(label)? + label.len()..];
        rest.split_once(" ms")?.0.trim().parse().ok()
    };
    Some((
        after("the folder's contents ")?,
        after("on the display by ")?,
    ))
}

pub fn round(fixture: &Path) -> Result<Vec<Measured>, String> {
    let binary = mcp::app()?;
    // This build's fsearch, which its app links too, must read the index
    // another commit's build may have generated, or the app would crawl.
    let index = search::index_dir(fixture).join("index.bin");
    let entries = fsearch::index::Index::load(&index).map(|index| index.n);
    let expected = search::expected_entries(fixture)?;
    if entries != Some(expected) {
        return Err(format!(
            "{}: this build reads {entries:?} entries, not {expected}, so its app would crawl",
            index.display()
        ));
    }
    let work = fixture.join(format!("session-{}", std::process::id()));
    std::fs::create_dir(&work).map_err(io(&work))?;
    let home = work.join("home");
    // A clone: the app saves its index, settings and session over these.
    mcp::cp_clone(&fixture.join("home"), &home)?;
    let short = ShortPath::to(&home, "home")?;
    let mut app = Command::new(&binary)
        .arg(fixture.join("opened"))
        .env("HOME", short.path())
        .env("MONODRIVE_UNATTENDED", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io(&binary))?;
    let (sender, lines) = mpsc::channel();
    if let Some(stderr) = app.stderr.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.starts_with("launch: ") && sender.send(line).is_err() {
                    return;
                }
            }
        });
    }
    let line = lines.recv_timeout(PATIENCE);
    let _ = app.kill();
    let _ = app.wait();
    drop(short);
    delete_fixture(&work)?;
    let line = line.map_err(|_| {
        format!(
            "{} printed no launch time in {PATIENCE:?}: build it with --features monodrive/profile",
            binary.display()
        )
    })?;
    let (contents, shown) = parse(&line).ok_or_else(|| format!("unreadable: {line}"))?;
    let measured = |bench: &str, title: &str, ms: f64| Measured {
        bench: bench.into(),
        title: title.into(),
        group: "Launch".into(),
        variant: String::new(),
        items: None,
        cache: Cache::Warm,
        sample: Sample {
            wall_ms: ms,
            cpu_ms: None,
            instructions: None,
            cycles: None,
        },
        check: format!("{OPENED} entries"),
        peak_bytes: None,
    };
    Ok(vec![
        measured(
            "contents",
            "Launch to drawing the folder's contents",
            contents,
        ),
        measured("shown", "Launch to the contents on the display", shown),
    ])
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_launch_line_reads() {
        let line = "launch: first frame drawn 120.4 ms, the folder's contents 121.0 ms, on the display by 160.9 ms after the process started";
        assert_eq!(super::parse(line), Some((121.0, 160.9)));
        assert_eq!(super::parse("launch: unknown"), None);
    }
}
