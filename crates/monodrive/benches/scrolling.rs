//! Scrolling a 10,000-entry folder in the list and the grid, through GPUI's
//! headless Metal renderer. Each run opens a fresh window, scrolls a fixed
//! distance one wheel event per frame, and reports frame and draw times.
//! A last run carries a file over the list instead, one move per frame, with
//! the pill and the outline drawn.
//! `cargo bench -p monodrive --bench scrolling`
//!
//! Benchmarks have no asset source, so the chrome's SVG icons draw nothing;
//! file type icons come from their own table and do draw.

use gpui::{
    BenchAppContext, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, PlatformInput,
    ScrollDelta, ScrollWheelEvent, TouchPhase, point, px,
};
use monodrive::browser::Browser;
use monodrive::preferences::{Preferences, View};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const ENTRIES: usize = 10_000;
/// Wheel events per run, each one frame.
const FRAMES: u64 = 120;
/// About three list rows per event, a brisk trackpad flick.
const STEP: f32 = 90.;

fn fill(dir: &Path) {
    const EXTS: [&str; 6] = ["jpg", "pdf", "png", "md", "txt", "rs"];
    for i in 0..ENTRIES {
        let path = dir.join(format!("file {i}.{}", EXTS[i % EXTS.len()]));
        std::fs::write(path, []).unwrap();
    }
}

fn scroll(view: View, cx: &mut BenchAppContext) {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    fill(dir.path());
    cx.update(|cx| {
        cx.set_global(Preferences {
            view,
            // Pictures would decode from empty files; this measures icons.
            thumbnails: false,
            ..Preferences::default()
        });
    });
    cx.run_until_idle();
    cx.bench_renderer_session(
        Duration::from_secs(30),
        |cx| {
            // Sessions don't close their windows, so the last run's goes here,
            // untimed, before it can draw alongside this one.
            cx.update(|cx| {
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
            });
            cx.run_until_idle();
            let mut window = cx.add_empty_window();
            let folder = dir.path().to_path_buf();
            window.update(|window, cx| {
                window.replace_root(cx, |_, cx| Browser::new(folder, cx));
            });
            // The listing lands and the first frame draws untimed.
            window.run_until_idle();
            let handle = window.window_handle();
            let stop = Arc::new(AtomicBool::new(false));
            (stop.clone(), handle, stop)
        },
        |stop, frame, window, cx| {
            let size = window.viewport_size();
            window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollWheelEvent {
                    // Over the listing, clear of the sidebar.
                    position: point(size.width * 0.6, size.height * 0.5),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-STEP))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                }),
                cx,
            );
            if frame + 1 >= FRAMES {
                stop.store(true, Ordering::Release);
            }
        },
    );
}

fn drag(cx: &mut BenchAppContext) {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    fill(dir.path());
    std::fs::create_dir(dir.path().join("A folder")).unwrap();
    cx.update(|cx| {
        cx.set_global(Preferences {
            thumbnails: false,
            ..Preferences::default()
        });
    });
    cx.run_until_idle();
    // Over the name of the third row, then up and down the rows above it.
    let at = |row: f32| point(px(300.), px(73. + 28. * (row + 0.5)));
    let moved = move |row: f32| {
        PlatformInput::MouseMove(MouseMoveEvent {
            position: at(row),
            pressed_button: Some(MouseButton::Left),
            modifiers: Modifiers::default(),
        })
    };
    cx.bench_renderer_session(
        Duration::from_secs(30),
        |cx| {
            cx.update(|cx| {
                for window in cx.windows() {
                    let _ = window.update(cx, |_, window, cx| {
                        cx.stop_active_drag(window);
                        window.remove_window();
                    });
                }
            });
            cx.run_until_idle();
            let mut window = cx.add_empty_window();
            let folder = dir.path().to_path_buf();
            window.update(|window, cx| {
                window.replace_root(cx, |_, cx| Browser::new(folder, cx));
            });
            window.run_until_idle();
            // The press and the move that starts the drag draw untimed.
            window.update(|window, cx| {
                window.dispatch_event(
                    PlatformInput::MouseDown(MouseDownEvent {
                        button: MouseButton::Left,
                        position: at(2.),
                        modifiers: Modifiers::default(),
                        click_count: 1,
                        first_mouse: false,
                    }),
                    cx,
                );
            });
            window.run_until_idle();
            window.update(|window, cx| window.dispatch_event(moved(1.5), cx));
            window.run_until_idle();
            let handle = window.window_handle();
            let stop = Arc::new(AtomicBool::new(false));
            (stop.clone(), handle, stop)
        },
        |stop, frame, window, cx| {
            // Target and pill change each frame, as a fast hand makes them.
            window.dispatch_event(moved((frame % 2) as f32), cx);
            if frame + 1 >= FRAMES {
                stop.store(true, Ordering::Release);
            }
        },
    );
}

#[gpui::bench]
fn scroll_list(cx: &mut BenchAppContext) {
    scroll(View::List, cx);
}

#[gpui::bench]
fn scroll_grid(cx: &mut BenchAppContext) {
    scroll(View::Grid, cx);
}

#[gpui::bench]
fn drag_over_list(cx: &mut BenchAppContext) {
    drag(cx);
}

gpui::bench_group!(benches, scroll_list, scroll_grid, drag_over_list);
gpui::bench_main!(benches);
