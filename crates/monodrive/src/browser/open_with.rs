//! Running what Open With asks: the window's half of `native`, with what went
//! wrong in a toast. Which apps are offered is `native::offered`.

use super::{Browser, OpenWith, Places};
use crate::jobs;
use crate::native::native;
use gpui::{Context, Task, Window};
use std::path::{Path, PathBuf};

impl Browser {
    /// Opens `files` in the app at `app`, off the UI thread, since macOS takes
    /// a moment to launch it.
    pub(super) fn open_files_with(
        &mut self,
        files: Vec<PathBuf>,
        app: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let native = native(cx);
        let places = cx.try_global::<Places>().cloned();
        let opened = cx.background_executor().spawn(async move {
            let opened = native.open_with(&files, &app);
            if let (Ok(()), Some(places)) = (&opened, places) {
                for file in &files {
                    let _ = places.store.opened(file);
                }
            }
            opened
        });
        self.say_if_failed(opened, cx);
    }

    /// Other…: asks for an app, then opens `files` in it.
    pub(super) fn open_files_with_other(&mut self, files: Vec<PathBuf>, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        let (send, chosen) = async_channel::bounded(1);
        native(cx).choose_app(Box::new(move |app| {
            let _ = send.try_send(app);
        }));
        cx.spawn(async move |_, cx| {
            if let Ok(Some(app)) = chosen.recv().await {
                let _ = this.update(cx, |this, cx| this.open_files_with(files, app, cx));
            }
        })
        .detach();
    }

    /// Always Open With: changes what opens every file of this type, so it
    /// says so and asks first.
    pub(super) fn ask_always_open_with(
        &mut self,
        files: Vec<PathBuf>,
        app: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(like) = files.into_iter().next() else {
            return;
        };
        let name = app_name(&app);
        let kind = kind_of(&like);
        let answer = window.prompt(
            gpui::PromptLevel::Info,
            &format!("Always open {kind} with {name}?"),
            Some("This changes the default app for every file of this type, not just this one."),
            &["Change All", "Cancel"],
            cx,
        );
        let native = native(cx);
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let said = format!("{name} now opens {kind}");
            let changed = cx
                .background_executor()
                .spawn(async move { native.make_default(&app, &like) })
                .await;
            let _ = this.update(cx, |this, cx| match changed {
                Ok(()) => this.toast(said, false, None, cx),
                Err(why) => this.toast(why, true, None, cx),
            });
        })
        .detach();
    }

    fn say_if_failed(&self, done: Task<Result<(), String>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            if let Err(why) = done.await {
                let _ = this.update(cx, |this, cx| this.toast(why, true, None, cx));
            }
        })
        .detach();
    }

    /// The File menu's and a shortcut's way in: the context menu on the
    /// selection, with the submenu open. AppKit's menu bar copies its items
    /// when it's built, so it can't list the apps for what's selected now.
    pub(super) fn open_with_menu(&mut self, _: &OpenWith, cx: &mut Context<Self>) {
        let (scopes, at, origin) = self.pane().update(cx, |pane, cx| {
            (pane.scopes(), pane.menu_point(cx), pane.list_focus.clone())
        });
        self.request_menu(scopes, at, origin, cx);
        if let Some(request) = &mut self.menu_request {
            request.open_with = true;
        }
    }
}

fn app_name(app: &Path) -> String {
    app.file_stem().map_or_else(
        || app.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What a file's type is called in Always Open With's question.
fn kind_of(file: &Path) -> String {
    match file.extension() {
        Some(extension) => format!(".{} files", extension.to_string_lossy()),
        None => format!("files like {}", jobs::quoted(file)),
    }
}
