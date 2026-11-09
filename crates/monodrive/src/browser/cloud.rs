//! A pane's side of cloud downloads: the badge that downloads, Download and
//! Remove Download, opening what isn't downloaded, and listing again once a
//! download lands. `downloads` keeps what's on its way for every pane.

use super::Offer;
use super::pane::Pane;
use super::short_reason;
use crate::downloads::{self, Changed, Downloads, State, Then};
use crate::jobs::quoted;
use crate::theme::{self, content, danger, icon};
use gpui::{AnyElement, App, Context, Entity, Hsla, div, prelude::*, px};
use monodrive_core::cloud::{Availability, CloudFolder};
use monodrive_core::entries::Entry;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Hears the app's downloads: repaints for their badges and lists again what
/// one changed.
pub(super) fn follow(cx: &mut Context<Pane>) -> Entity<Downloads> {
    let downloads = downloads::get(cx);
    cx.observe(&downloads, |_, _, cx| cx.notify()).detach();
    cx.subscribe(&downloads, |pane, _, Changed(path), cx| {
        if pane
            .entries
            .iter()
            .any(|entry| entry.path.starts_with(path))
        {
            pane.list(true, cx);
        }
    })
    .detach();
    downloads
}

impl Pane {
    /// Whether Download would do anything for what's selected.
    pub(super) fn can_download(&self, cx: &App) -> bool {
        !self.downloadable(cx).is_empty()
    }

    /// What's selected that Remove Download could act on: downloaded, and in
    /// one of the `cloud` folders, not the folder itself. Whether it's
    /// uploaded is macOS's to say when asked.
    pub(super) fn removable(&self, cloud: &[CloudFolder]) -> Vec<PathBuf> {
        self.selection
            .in_order(&self.entries)
            .into_iter()
            .filter(|entry| entry.availability == Availability::Local)
            .filter(|entry| {
                cloud
                    .iter()
                    .any(|c| entry.path.starts_with(&c.path) && entry.path != c.path)
            })
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// What's selected that isn't all downloaded and isn't on its way.
    fn downloadable(&self, cx: &App) -> Vec<PathBuf> {
        let downloads = self.downloads.read(cx);
        self.selection
            .in_order(&self.entries)
            .into_iter()
            .filter(|entry| {
                downloads.state(&entry.path) != Some(&State::Running)
                    && (self.in_cloud(entry)
                        || matches!(downloads.state(&entry.path), Some(State::Failed(_))))
            })
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub(super) fn download_selected(&mut self, cx: &mut Context<Self>) {
        let paths = self.downloadable(cx);
        self.download(paths, cx);
    }

    /// Asks for `paths`, and says why any fails, with Try Again.
    pub(super) fn download(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let then = ended(cx);
        self.downloads
            .update(cx, |downloads, cx| downloads.download(paths, then, cx));
    }

    /// Opens the placeholder `path` in its app once it's downloaded, as Finder
    /// does. Handing it over now would leave the app waiting on the network
    /// with nothing on screen saying so.
    pub(super) fn download_and_open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // Opening it again while it's on its way opens it once.
        if !self.open_after_download.insert(path.clone()) {
            return;
        }
        let said = format!("Downloading {} to open it", quoted(&path));
        self.toast(said, false, None, cx);
        let then = ended(cx);
        self.downloads
            .update(cx, |downloads, cx| downloads.download(vec![path], then, cx));
    }

    /// Asks the provider to keep only placeholders of `paths`, then says what
    /// it did, with Download to bring them back, or why it couldn't.
    pub(super) fn remove_downloads_of(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let pane = cx.weak_entity();
        let done = Box::new(
            move |ended: Vec<(PathBuf, Result<(), String>)>, cx: &mut App| {
                let _ = pane.update(cx, |pane, cx| {
                    let (removed, failed): (Vec<_>, Vec<_>) =
                        ended.into_iter().partition(|(_, how)| how.is_ok());
                    let removed: Vec<PathBuf> = removed.into_iter().map(|(path, _)| path).collect();
                    if let Some((_, Err(why))) = failed.first() {
                        let why = short_reason(why, [pane.folder()]);
                        return pane.toast(why, true, None, cx);
                    }
                    let said = match removed.as_slice() {
                        [one] => format!("Removed the download of {}", quoted(one)),
                        many => format!("Removed the downloads of {} items", many.len()),
                    };
                    pane.toast(said, false, Some(Offer::Download("Download", removed)), cx);
                });
            },
        );
        self.downloads
            .update(cx, |downloads, cx| downloads.remove(paths, done, cx));
    }

    /// The badge after `entry`'s name, `side` across in `ink`: what a download
    /// of it is doing, or a button that downloads what isn't all here.
    pub(super) fn cloud_badge(
        &self,
        entry: &Entry,
        ix: usize,
        side: f32,
        ink: Hsla,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let state = self.downloads.read(cx).state(&entry.path).cloned();
        let (glyph, label, ink): (_, &'static str, _) = match &state {
            Some(State::Running) => {
                return Some(
                    div()
                        .id(("cloud-badge", ix))
                        .debug_selector(|| "cloud-downloading".into())
                        .role(gpui::Role::ProgressIndicator)
                        .aria_label("Downloading")
                        .flex_shrink_0()
                        .child(icon("cloud-downloading", side).text_color(ink))
                        .into_any_element(),
                );
            }
            Some(State::Failed(_)) => ("cloud-alert", "Try Again", danger()),
            None if self.in_cloud(entry) => ("cloud-download", "Download", ink),
            None => return None,
        };
        // A 20 pt circle around the glyph, as a row's icon button has, laid
        // over it so it takes no room: the name doesn't move.
        let pad = (20. - side) / 2.;
        let path = entry.path.clone();
        let button = div()
            .id(("cloud-badge", ix))
            .debug_selector(move || format!("cloud-badge-{ix}"))
            .role(gpui::Role::Button)
            .aria_label(label)
            .absolute()
            .top(px(-pad))
            .left(px(-pad))
            .size(px(20.))
            .rounded_full()
            .cursor_pointer()
            .hover(|d| d.bg(content(0.14)))
            .active(|d| d.bg(content(0.2)))
            // The press is the badge's, so it doesn't select, open or drag.
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |pane, _, _, cx| {
                cx.stop_propagation();
                pane.download(vec![path.clone()], cx);
            }));
        Some(
            div()
                .relative()
                .flex_shrink_0()
                .size(px(side))
                .child(theme::tip(button, label))
                .child(icon(glyph, side).text_color(ink))
                .into_any_element(),
        )
    }
}

/// What a pane does when its download ends: opens it if it was opened while
/// on its way, or says why it failed, with Try Again.
fn ended(cx: &Context<Pane>) -> Then {
    let pane = cx.weak_entity();
    Rc::new(move |path: &Path, how: Result<(), String>, cx: &mut App| {
        let _ = pane.update(cx, |pane, cx| match how {
            Ok(()) => {
                if pane.open_after_download.remove(path) {
                    pane.open_file(path, cx);
                }
            }
            Err(why) => {
                pane.open_after_download.remove(path);
                let why = format!(
                    "Couldn't download {}: {}",
                    quoted(path),
                    short_reason(&why, [pane.folder()])
                );
                let again = Offer::Download("Try Again", vec![path.to_path_buf()]);
                pane.toast(why, true, Some(again), cx);
            }
        });
    })
}
