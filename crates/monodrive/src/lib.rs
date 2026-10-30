//! The Monodrive app. `main.rs` decodes the command line; benchmarks build
//! views from here.

pub mod assets;
mod background;
pub mod browser;
mod commands;
pub mod file_icons;
mod frame;
mod goto;
mod palette;
mod palettes;
pub mod preferences;
#[cfg(feature = "profile")]
mod profile;
mod selection;
mod settings;
mod text_field;
mod theme;
mod thumbnails;
mod traffic_lights;

use commands::{CloseWindow, Hide, HideOthers, Minimize, Quit, ShowAll};
use gpui::{
    App, Focusable, TitlebarOptions, WindowBackgroundAppearance, WindowBounds, WindowOptions,
    point, prelude::*, px, size,
};
use preferences::Preferences;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Opens a window on `folder`, or the home folder, and starts the index.
pub fn run(folder: Option<PathBuf>) {
    let folder = folder
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| "/".into());
    let platform = gpui_platform::current_platform(false);
    gpui::Application::with_platform(platform)
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            #[cfg(feature = "profile")]
            profile::start(cx);
            preferences::init(cx);
            // Connects on the first undo or redo, starting the owner if needed.
            if let Some(paths) = monodrive_core::ops::Paths::user() {
                let client = monodrive_core::ops::Client::new(paths, std::env::current_exe().ok());
                cx.set_global(browser::Operations(Arc::new(Mutex::new(client))));
            }
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
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let screens: Vec<_> = cx
                .displays()
                .iter()
                .map(|display| display.bounds())
                .collect();
            // The part clear of the menu bar and the Dock, as `frame::apply` uses.
            let screen = cx.primary_display().map(|display| display.visible_bounds());
            let bounds = screen.map(|screen| {
                let prefs = cx.global::<Preferences>();
                WindowBounds::Windowed(frame::opening(
                    prefs.window_size,
                    prefs.window_frame,
                    screen,
                    &screens,
                ))
            });
            cx.open_window(
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
                    let view = cx.new(|cx| {
                        let mut browser = browser::Browser::new(folder, cx);
                        browser.start_index(cx);
                        browser
                    });
                    window.focus(&view.focus_handle(cx), cx);
                    traffic_lights::pin(window, theme::TITLE_BAR_HEIGHT * window.content_zoom());
                    // AppKit rebuilds the buttons on some titlebar changes.
                    view.update(cx, |_, cx| {
                        cx.observe_window_activation(window, |_, window, _| {
                            traffic_lights::pin(
                                window,
                                theme::TITLE_BAR_HEIGHT * window.content_zoom(),
                            )
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
            )
            .expect("open the Monodrive window");
            cx.activate(true);
        });
}
