//! The Monodrive app. `main.rs` decodes the command line; benchmarks build
//! views from here.

mod agents;
pub mod assets;
mod background;
pub mod browser;
mod commands;
pub mod connect;
mod empty_trash;
pub mod file_icons;
mod frame;
mod goto;
mod jobs;
mod menu;
mod palette;
mod palettes;
mod pasteboard;
pub mod preferences;
#[cfg(feature = "profile")]
mod profile;
mod quick_look;
mod selection;
pub mod session;
mod settings;
mod text_field;
mod theme;
mod thumbnails;
mod traffic_lights;
mod workspace;

use commands::{CloseWindow, Hide, HideOthers, Minimize, NewWindow, Quit, ShowAll};
use gpui::{
    App, Focusable, Global, TitlebarOptions, WindowBackgroundAppearance, WindowBounds,
    WindowOptions, point, prelude::*, px, size,
};
use preferences::Preferences;
use session::WindowState;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

/// Who hears what the app's one watch on something says: each window.
#[derive(Clone, Default)]
struct Hub(Arc<Mutex<Vec<async_channel::Sender<()>>>>);

impl Hub {
    /// Tells every window that's still listening.
    fn say(&self) {
        let mut heard = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        heard.retain(|window| window.try_send(()).is_ok());
    }

    fn listen(&self) -> async_channel::Receiver<()> {
        let (send, receive) = async_channel::unbounded();
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(send);
        receive
    }
}

/// What every window shares: the watches, kept for as long as the app runs.
struct Shared {
    jobs: Hub,
    volumes: Hub,
    lists: Hub,
    _watches: Box<dyn std::any::Any>,
}

impl Global for Shared {}

/// What a window opens on.
enum Opening {
    Folder(PathBuf),
    Restored(WindowState),
}

/// Opens a window, or those of the last session, and starts the index. With a
/// `folder` the session stays as it was, for the next launch without one.
pub fn run(folder: Option<PathBuf>) {
    let platform = gpui_platform::current_platform(false);
    gpui::Application::with_platform(platform)
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            #[cfg(feature = "profile")]
            profile::start(cx);
            preferences::init(cx);
            cx.set_global(browser::SharedIndex(Arc::default()));
            let (jobs, volumes, lists) = (Hub::default(), Hub::default(), Hub::default());
            // Connects on the first change or look at the jobs, starting the
            // owner if needed. The watch on its journal connects to nothing.
            let mut job_watch = None;
            if let Some(paths) = monodrive_core::ops::Paths::user() {
                let client = monodrive_core::ops::Client::new(paths, std::env::current_exe().ok());
                let heard = jobs.clone();
                job_watch = client.watch(move || heard.say()).ok();
                cx.set_global(browser::Operations(Arc::new(Mutex::new(client))));
            }
            let mut place_watches = None;
            if let (Some(store), Some(paths)) = (
                monodrive_core::places::Store::user(),
                monodrive_core::ops::Paths::user(),
            ) {
                let (heard_volumes, heard_lists) = (volumes.clone(), lists.clone());
                place_watches = Some((
                    monodrive_core::volumes::watch(move || heard_volumes.say()).ok(),
                    store.watch(move || heard_lists.say()).ok(),
                ));
                cx.set_global(browser::Places {
                    store,
                    trash: paths.trash,
                    drives: Arc::new(browser::System),
                    applications: vec![
                        "/Applications".into(),
                        browser::home().join("Applications"),
                    ],
                });
            }
            cx.set_global(Shared {
                jobs,
                volumes,
                lists,
                _watches: Box::new((job_watch, place_watches)),
            });
            cx.set_global(pasteboard::Pasteboard::general());
            commands::apply(cx);
            // A changed shortcut rebuilds the keymap and the menu bar; Show
            // Hidden Files' checkmark rebuilds the menu bar alone.
            cx.observe_global::<Preferences>(|cx| {
                commands::apply(cx);
                commands::refresh_menus(cx);
            })
            .detach();
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.on_action(|_: &Hide, cx| cx.hide());
            cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
            cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
            cx.on_action(|_: &CloseWindow, cx| {
                if let Some(window) = cx.active_window() {
                    let _ = window.update(cx, |_, window, _| window.remove_window());
                }
            });
            cx.on_action(|_: &Minimize, cx| {
                if let Some(window) = cx.active_window() {
                    let _ = window.update(cx, |_, window, _| window.minimize_window());
                }
            });
            cx.on_action(|_: &NewWindow, cx| {
                open_window(Opening::Folder(browser::home()), false, cx);
                cx.activate(true);
            });
            cx.on_window_closed(|cx, window| {
                session::forget(window.as_u64(), cx);
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let openings = match folder {
                Some(folder) => vec![Opening::Folder(folder)],
                None => {
                    let saved = session::path().and_then(|path| {
                        let saved = session::load(&path);
                        session::init(path, cx);
                        saved
                    });
                    match saved {
                        Some(saved) => saved.windows.into_iter().map(Opening::Restored).collect(),
                        None => vec![Opening::Folder(browser::home())],
                    }
                }
            };
            let several = openings.len() > 1;
            for opening in openings {
                open_window(opening, several, cx);
            }
            cx.activate(true);
        });
}

/// Opens a window, moved along from the ones already showing. Restoring
/// `several`, each goes back where it was, or they'd all land on the same spot.
fn open_window(opening: Opening, several: bool, cx: &mut App) {
    let screens: Vec<_> = cx
        .displays()
        .iter()
        .map(|display| display.bounds())
        .collect();
    let others = cx.windows().len();
    let saved_frame = match &opening {
        Opening::Restored(state) if several => state.frame,
        _ => None,
    };
    // The part clear of the menu bar and the Dock, as `frame::apply` uses.
    let screen = cx.primary_display().map(|display| display.visible_bounds());
    let bounds = screen.map(|screen| {
        let prefs = cx.global::<Preferences>();
        let size = if saved_frame.is_some() {
            preferences::WindowSize::LastUsed
        } else {
            prefs.window_size
        };
        let last = saved_frame.or(prefs.window_frame);
        let bounds = frame::opening(size, last, screen, &screens);
        WindowBounds::Windowed(if saved_frame.is_some() {
            bounds
        } else {
            frame::cascaded(bounds, others)
        })
    });
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: bounds,
            window_min_size: Some(size(px(frame::MIN_WIDTH), px(frame::MIN_HEIGHT))),
            window_background: WindowBackgroundAppearance::Blurred,
            // GPUI draws the titlebar; AppKit must not treat its
            // buttons as drag or double-click targets.
            app_owns_titlebar_drag: true,
            titlebar: Some(TitlebarOptions {
                title: Some("Monodrive".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(
                    px(12.),
                    px((theme::TITLE_BAR_HEIGHT - 14.) / 2.),
                )),
            }),
            ..Default::default()
        },
        |window, cx| {
            let key = window.window_handle().window_id().as_u64();
            let view = cx.new(|cx| {
                let mut browser = match opening {
                    Opening::Folder(folder) => browser::Browser::new(folder, cx),
                    Opening::Restored(state) => browser::Browser::restored(state, cx),
                };
                browser.start_index(cx);
                let (jobs, volumes, lists) = {
                    let shared = cx.global::<Shared>();
                    (
                        shared.jobs.listen(),
                        shared.volumes.listen(),
                        shared.lists.listen(),
                    )
                };
                browser.hear_jobs(jobs, cx);
                browser.hear_places((), volumes, lists, cx);
                browser.keep_session(key, cx);
                browser
            });
            window.focus(&view.focus_handle(cx), cx);
            traffic_lights::pin(window, theme::TITLE_BAR_HEIGHT * window.content_zoom());
            // AppKit rebuilds the buttons on some titlebar changes.
            view.update(cx, |_, cx| {
                cx.observe_window_activation(window, |browser, window, cx| {
                    traffic_lights::pin(window, theme::TITLE_BAR_HEIGHT * window.content_zoom());
                    if window.is_window_active() {
                        // Files may have come and gone while away.
                        browser.load_volumes(cx);
                        browser.window_activated(cx);
                    }
                })
                .detach();
                // The System theme follows macOS's appearance.
                cx.observe_window_appearance(window, |_, _, cx| cx.notify())
                    .detach();
            });
            // Kept always, so choosing Last Used works at once.
            view.update(cx, |browser, cx| browser.track_frame(window, cx));
            window.activate_window();
            window.on_next_frame(|_, cx| commands::menus_ready(cx));
            #[cfg(feature = "profile")]
            profile::on_launch(&view, window, cx);
            view
        },
    );
    opened.expect("open the Monodrive window");
}
