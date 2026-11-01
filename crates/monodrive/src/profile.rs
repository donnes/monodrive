//! The `profile` feature: prints how long launch took once the folder's
//! contents are drawn, and how long frames took to draw when the app quits.
//! Measure release builds.

use crate::browser::Browser;
use gpui::{App, Entity, FrameEvent, FrameTiming, FrameTimingCollector, Global, Window};
use std::time::{Duration, Instant, SystemTime};

struct Profile {
    collector: FrameTimingCollector,
    draws: Vec<FrameTiming>,
}

impl Global for Profile {}

impl Profile {
    fn collect(&mut self) -> &[FrameTiming] {
        self.draws
            .extend(
                self.collector
                    .collect_unseen()
                    .into_iter()
                    .filter_map(|event| match event {
                        FrameEvent::Draw(timing) => Some(timing),
                        FrameEvent::Present(_) => None,
                    }),
            );
        &self.draws
    }
}

pub fn start(cx: &mut App) {
    gpui::set_trace_enabled(true);
    cx.set_global(Profile {
        collector: FrameTimingCollector::new(),
        draws: Vec::new(),
    });
    cx.on_app_quit(|cx| {
        report_frames(cx.global_mut::<Profile>().collect());
        async {}
    })
    .detach();
}

/// Prints how long launch took once the window shows the folder's contents:
/// to the first frame, to the first frame after the listing landed, and to the
/// frame signal after that one, by when it has been presented. Launch usually
/// lists before the window opens, so the first frame has them.
pub fn on_launch(browser: &Entity<Browser>, window: &mut Window, cx: &mut App) {
    if !browser.read(cx).is_listing(cx) {
        return report_after_next_frame(Instant::now(), window);
    }
    let mut landed = false;
    window
        .observe(browser, cx, move |browser, window, cx| {
            if landed || browser.read(cx).is_listing(cx) {
                return;
            }
            landed = true;
            report_after_next_frame(Instant::now(), window);
        })
        .detach();
}

/// The next frame signal presents the listing; the one after can report it.
fn report_after_next_frame(landed: Instant, window: &mut Window) {
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |_, cx| print_launch(landed, cx))
    });
}

fn print_launch(landed: Instant, cx: &mut App) {
    let shown = since_process_start();
    let draws = cx.global_mut::<Profile>().collect();
    let since_start = |at: Instant| since_process_start()?.checked_sub(at.elapsed());
    let first = draws.first().and_then(|t| since_start(t.draw_end));
    let contents = draws
        .iter()
        .find(|t| t.draw_start >= landed)
        .and_then(|t| since_start(t.draw_end));
    match (first, contents, shown) {
        (Some(first), Some(contents), Some(shown)) => eprintln!(
            "launch: first frame drawn {first:.1?}, the folder's contents {contents:.1?}, on the display by {shown:.1?} after the process started"
        ),
        _ => eprintln!("launch: unknown"),
    }
}

fn report_frames(draws: &[FrameTiming]) {
    let mut times: Vec<Duration> = draws.iter().map(|t| t.draw_end - t.draw_start).collect();
    if times.is_empty() {
        return;
    }
    times.sort();
    let at = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    let budget = draws.iter().find_map(|t| t.refresh_interval);
    let over = budget.map_or(0, |budget| times.iter().filter(|&&t| t > budget).count());
    eprintln!(
        "frames: {} drawn; p50 {:.2?}, p95 {:.2?}, p99 {:.2?}, max {:.2?}; {over} over the {:.2?} refresh",
        times.len(),
        at(0.5),
        at(0.95),
        at(0.99),
        at(1.0),
        budget.unwrap_or_default(),
    );
}

/// How long ago the kernel started this process, so launch counts loading
/// the binary too.
fn since_process_start() -> Option<Duration> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: `info` is a writable buffer of exactly `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            std::process::id() as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the kernel filled all of it.
    let info = unsafe { info.assume_init() };
    let started = SystemTime::UNIX_EPOCH
        + Duration::from_secs(info.pbi_start_tvsec)
        + Duration::from_micros(info.pbi_start_tvusec);
    SystemTime::now().duration_since(started).ok()
}
