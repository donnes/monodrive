//! Opening what's selected beyond the list: in the pane's preview or Quick
//! Look, in its app or the one Open With picks, and a server's file coming
//! down first, with the toast that follows it.

use super::Browser;
use crate::commands::OpenWith;
use crate::jobs;
use crate::native::native;
use crate::preferences::Preferences;
use crate::preview::{PreviewView, Saved};
use crate::quick_look::{self, Preview};
use crate::system::Places;
use crate::system::reach;
use crate::theme::{self, ButtonLook, content, text_button};
use gpui::{Context, Global, Task, Window, div, prelude::*, px};
use monodrive_core::remote::{
    Meter, PREVIEW_LIMIT, Preview as RemotePreview, Previews, RemoteError, RemotePath,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// How often the download toast redraws while a file comes down.
const FETCH_TICK: std::time::Duration = std::time::Duration::from_millis(150);

/// A file on a server coming down to open or Quick Look.
pub(super) struct Fetching {
    name: String,
    size: u64,
    copied: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    _work: Task<()>,
    _tick: Task<()>,
}

/// The app's preview cache, and the downloads it keeps for as long as the
/// app runs, since an app may still have them open. Quitting removes them.
#[derive(Default)]
struct Downloads {
    cache: Option<Arc<Previews>>,
    kept: Vec<RemotePreview>,
}

impl Global for Downloads {}

impl Browser {
    /// Downloads `at`, `size` bytes as listed, into the preview cache, then
    /// opens it in its app, or in Quick Look with `look`. A download on its
    /// way stops for the new one.
    pub(super) fn fetch(&mut self, at: RemotePath, size: u64, look: bool, cx: &mut Context<Self>) {
        if let Some(old) = self.fetching.take() {
            old.cancel.store(true, Ordering::Relaxed);
        }
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return;
        };
        self.toast = None;
        let (copied, cancel) = (
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicBool::new(false)),
        );
        let cache = cx.default_global::<Downloads>().cache.clone();
        let reach = reach(at.source(), cx);
        let name = at.name().unwrap_or_default().to_owned();
        let work = {
            let (copied, cancel) = (copied.clone(), cancel.clone());
            cx.spawn(async move |this, cx| {
                let fetched = match reach.await {
                    Ok(open) => {
                        let root = places.previews.clone();
                        cx.background_executor()
                            .spawn(async move {
                                let cache = match cache {
                                    Some(cache) => cache,
                                    None => Arc::new(
                                        Previews::open(&root, PREVIEW_LIMIT)
                                            .map_err(RemoteError::Failed)?,
                                    ),
                                };
                                let meter = Meter {
                                    copied: &copied,
                                    cancel: &cancel,
                                };
                                let preview = cache.fetch(&open, &at, &meter)?;
                                Ok((cache, preview))
                            })
                            .await
                    }
                    Err(error) => Err(error),
                };
                let _ = this.update(cx, |this, cx| {
                    this.fetching = None;
                    match fetched {
                        Ok((cache, preview)) => {
                            let path = preview.path.clone();
                            let downloads = cx.default_global::<Downloads>();
                            downloads.cache.get_or_insert(cache);
                            downloads.kept.push(preview);
                            // The panel needs the window, which the next frame has.
                            if look {
                                this.look_at = Some(path);
                            } else {
                                places.drives.open(&path, cx);
                            }
                        }
                        Err(RemoteError::Cancelled) => {}
                        Err(error) => this.toast(error.to_string(), true, None, cx),
                    }
                    cx.notify();
                });
            })
        };
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FETCH_TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        self.fetching = Some(Fetching {
            name,
            size,
            copied,
            cancel,
            _work: work,
            _tick: tick,
        });
        cx.notify();
    }

    /// Apple's panel on a downloaded file. It isn't the selection's, so the
    /// panel isn't kept following it.
    pub(super) fn quick_look_file(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_preview(cx);
        if self.quick_look.is_none() {
            self.quick_look = crate::quick_look::Panel::new(window);
        }
        if let Some(panel) = &self.quick_look {
            let preview = crate::quick_look::Preview {
                items: vec![(path, false)],
                index: 0,
            };
            panel.show(&preview, cx);
        }
    }

    /// The download on its way, where a toast shows: what, how far, Cancel.
    pub(super) fn fetching_view(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<gpui::Stateful<gpui::Div>> {
        let fetching = self.fetching.as_ref()?;
        let copied = fetching.copied.load(Ordering::Relaxed);
        let fraction = if fetching.size == 0 {
            0.
        } else {
            (copied as f32 / fetching.size as f32).min(1.)
        };
        Some(
            theme::floating(theme::Elevation::Popover)
                .id("fetching")
                .debug_selector(|| "fetching".into())
                .occlude()
                .max_w(px(520.))
                .mx_4()
                .h(px(36.))
                .pl_3()
                .pr(px(6.))
                .flex()
                .items_center()
                .gap_3()
                .text_size(px(theme::TEXT_BODY))
                .child(
                    div()
                        .relative()
                        .min_w_0()
                        .truncate()
                        .child(format!("Downloading \"{}\"", fetching.name)),
                )
                .child(
                    theme::progress_track()
                        .relative()
                        .w(px(90.))
                        .flex_shrink_0()
                        .child(theme::progress_fill(fraction)),
                )
                .child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(format!(
                            "{} of {}",
                            crate::format::format_bytes(copied),
                            crate::format::format_bytes(fetching.size)
                        )),
                )
                .child(
                    text_button("fetching-cancel", "Cancel", ButtonLook::Ghost)
                        .debug_selector(|| "fetching-cancel".into())
                        .relative()
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(fetching) = this.fetching.take() {
                                fetching.cancel.store(true, Ordering::Relaxed);
                            }
                            cx.notify();
                        })),
                ),
        )
    }

    /// The download's card, where a toast sits, above the path bar.
    pub(super) fn fetching_toast(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let card = self.fetching_view(cx)?;
        let bar = if cx.global::<Preferences>().show_path_bar {
            theme::PATH_BAR_HEIGHT
        } else {
            0.
        };
        Some(
            div()
                .absolute()
                .bottom(px(16. + bar))
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(card),
        )
    }
}

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

impl Browser {
    /// Space and ⌘Y: the selection's preview in the pane. Once it's open they
    /// play or pause what it plays, and Esc closes it. Apple's panel still
    /// closes on them, as it does in Finder.
    pub(super) fn preview_or_play(&mut self, cx: &mut Context<Self>) {
        if let Some((_, view)) = &self.previewing {
            view.update(cx, |view, cx| view.toggle_playing(cx));
            return;
        }
        if self.close_preview(cx) {
            return;
        }
        let Some(preview) = self.pane().read(cx).preview_of_selection() else {
            return;
        };
        self.show_in_pane(&preview, cx);
        self.preview = Some(preview);
    }

    /// Apple's panel, for what the pane's preview doesn't draw well.
    pub(super) fn open_in_quick_look(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tools_kept_it(cx) {
            return;
        }
        self.close_preview(cx);
        let Some(preview) = self.pane().read(cx).preview_of_selection() else {
            return;
        };
        if self.quick_look.is_none() {
            self.quick_look = quick_look::Panel::new(window);
        }
        if let Some(panel) = &self.quick_look {
            panel.show(&preview, cx);
        }
        self.in_panel = true;
        self.preview = Some(preview);
    }

    pub(super) fn show_in_pane(&mut self, preview: &Preview, cx: &mut Context<Self>) {
        let Some((path, folder)) = preview.items.get(preview.index).cloned() else {
            return;
        };
        let pane = self.pane().clone();
        let view = match self
            .previewing
            .as_ref()
            .filter(|(on, _)| on.entity_id() == pane.entity_id())
        {
            Some((_, view)) => view.clone(),
            None => {
                let view = cx.new(|_| PreviewView::new());
                // Saving reports through the pane's toast, with its Undo.
                let saved_in = pane.downgrade();
                cx.subscribe(&view, move |_, _, saved: &Saved, cx| {
                    let done = saved.0.clone();
                    let _ = saved_in.update(cx, |pane, cx| pane.settle(done, false, cx));
                })
                .detach();
                // Once changes are saved or discarded, it follows again.
                cx.observe(&view, |this, _, cx| this.follow_selection(cx))
                    .detach();
                pane.update(cx, |pane, cx| pane.show_preview(Some(view.clone()), cx));
                self.previewing = Some((pane.downgrade(), view.clone()));
                view
            }
        };
        let position = (preview.index, preview.items.len());
        view.update(cx, |view, cx| view.show(&path, folder, position, cx));
    }

    /// Leaving the preview leaves its tools first, which ask before losing
    /// changes. Says whether that used the key.
    pub(super) fn tools_kept_it(&mut self, cx: &mut Context<Self>) -> bool {
        self.previewing
            .as_ref()
            .is_some_and(|(_, view)| view.update(cx, |view, cx| view.escape(cx)))
    }

    /// Closes the preview, saying whether it was open.
    pub(super) fn close_preview(&mut self, cx: &mut Context<Self>) -> bool {
        // The panel also closes itself, on Esc or its close button.
        let in_panel = std::mem::take(&mut self.in_panel);
        let open = self.preview.take().is_some()
            && (!in_panel || self.quick_look.as_ref().is_none_or(|panel| panel.is_open()));
        if in_panel && let Some(panel) = &self.quick_look {
            panel.close();
        }
        if let Some((pane, view)) = self.previewing.take() {
            view.update(cx, |view, cx| view.release(cx));
            let _ = pane.update(cx, |pane, cx| pane.show_preview(None, cx));
        }
        open
    }

    /// While a preview is open it shows what's selected now, and closes when
    /// nothing is, as on going into another folder, or the keys move to the
    /// other pane.
    pub(super) fn follow_selection(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = &self.preview else {
            return;
        };
        let preview = self.pane().read(cx).preview_of_selection();
        let moved_pane = self
            .previewing
            .as_ref()
            .is_some_and(|(on, _)| on.entity_id() != self.pane().entity_id());
        if preview.as_ref() == Some(shown) && !moved_pane {
            return;
        }
        // Unsaved changes stay on screen wherever the selection or keys go, and
        // the move lands when they're done with.
        if self
            .previewing
            .as_ref()
            .is_some_and(|(_, view)| view.read(cx).holding())
        {
            if let Some(preview) = preview.filter(|_| !moved_pane) {
                self.show_in_pane(&preview, cx);
                self.preview = Some(preview);
            }
            return;
        }
        // Not if the panel closed itself meanwhile.
        let open = !self.in_panel || self.quick_look.as_ref().is_none_or(|panel| panel.is_open());
        match preview.filter(|_| open && !moved_pane) {
            Some(preview) => {
                if self.in_panel {
                    if let Some(panel) = &self.quick_look {
                        panel.show(&preview, cx);
                    }
                } else {
                    self.show_in_pane(&preview, cx);
                }
                self.preview = Some(preview);
            }
            None => {
                self.close_preview(cx);
            }
        }
    }
}
