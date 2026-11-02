//! One real window holding the preview of a file, to check what headless
//! snapshots can't draw: native views, playback and keys. No index, no session,
//! no owner. Prints each key it gets, which shows the keys came back after a
//! click inside a native view.
//!
//! `cargo run -p monodrive --example preview_window -- <file>`

use gpui::{App, Entity, FocusHandle, Window, WindowOptions, div, prelude::*};
use monodrive::assets::Assets;
use monodrive::preferences::Preferences;
use monodrive::preview::PreviewView;
use std::path::PathBuf;

struct Shell {
    view: Entity<PreviewView>,
    keys: FocusHandle,
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.keys)
            .on_key_down(|event, _, _| println!("key: {}", event.keystroke.key))
            .size_full()
            .flex()
            .flex_col()
            .child(self.view.clone())
    }
}

fn main() {
    let path = PathBuf::from(std::env::args().nth(1).expect("a file to preview"));
    gpui::Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            cx.set_global(Preferences::default());
            let folder = path.is_dir();
            cx.open_window(WindowOptions::default(), move |window, cx| {
                let view = cx.new(|cx| {
                    let mut view = PreviewView::new();
                    view.show(&path, folder, (0, 1), cx);
                    view
                });
                let keys = cx.focus_handle();
                window.focus(&keys, cx);
                cx.new(|_| Shell { view, keys })
            })
            .expect("a window");
            cx.activate(true);
        });
}
