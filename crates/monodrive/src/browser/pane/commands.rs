//! What a pane's commands do to what's selected: open it, make, copy and
//! send files, share them, and download cloud placeholders.

use super::{with_owner, *};
use crate::browser::Offer;
use crate::browser::pane::Pane;
use crate::downloads::{self, Changed, Downloads, State, Then};
use crate::format::short_reason;
use crate::jobs::quoted;
use crate::pasteboard::Pasteboard;
use crate::preferences::Preferences;
use crate::{jobs, workspace};
use gpui::{App, ClipboardItem, Context, Entity, Pixels, Point, Window};
use monodrive_core::cloud::{self as icloud, CloudFolder};
use monodrive_core::entries::Kind;
use monodrive_core::ops::{Conflict, Request};
use monodrive_core::{Origin, disk, info};
use std::path::{Path, PathBuf};
use std::rc::Rc;

impl Pane {
    /// Open in Terminal: at each selected folder and the folder holding
    /// each other item, or here with nothing selected.
    pub(in crate::browser) fn open_in_terminal(&mut self, cx: &mut Context<Self>) {
        let mut folders: Vec<PathBuf> = Vec::new();
        for entry in self.selection.in_order(&self.entries) {
            let folder = match entry.kind {
                Kind::Dir => Some(entry.path.as_path()),
                _ => entry.path.parent(),
            };
            if let Some(folder) = folder.filter(|f| !folders.iter().any(|seen| seen == f)) {
                folders.push(folder.to_path_buf());
            }
        }
        if folders.is_empty() {
            folders.push(self.disk_folder().to_path_buf());
        }
        let terminal = cx.global::<Preferences>().terminal;
        if let Err(e) = workspace::open_in_terminal(&folders, &terminal) {
            self.toast(e, true, None, cx);
        }
    }
    /// Show Package Contents: goes inside the selected package, which Open
    /// launches instead. A link goes in only if it leads to a folder, which is
    /// on the disk, maybe a slow volume.
    pub(in crate::browser) fn show_package_contents(&mut self, cx: &mut Context<Self>) {
        let Some(package) = self.package() else {
            return;
        };
        let target = package.clone();
        let is_dir = cx
            .background_executor()
            .spawn(async move { target.is_dir() });
        let asked_on = self.selected_now();
        self.opening = cx.spawn(async move |this, cx| {
            let is_dir = is_dir.await;
            let _ = this.update(cx, |this, cx| {
                if is_dir && this.selected_now() == asked_on {
                    this.navigate(package, cx);
                }
            });
        });
    }
    /// Opens what's selected: a lone folder goes in, and files and packages
    /// open in their apps. Several folders at once would need tabs or windows. Past
    /// `OPEN_WITHOUT_ASKING` files it asks first, as Finder does, so ⌘A then
    /// ↩ can't launch a whole folder.
    pub(in crate::browser) fn open_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.showing_worktrees() {
            return self.open_worktree(window, cx);
        }
        if self.showing_duplicates() {
            return self.open_duplicate(window, cx);
        }
        if self.batch.is_some() {
            return;
        }
        let chosen = self.selection.in_order(&self.entries);
        if let [only] = chosen.as_slice() {
            let ix = self.entries.iter().position(|e| e.path == only.path);
            if let Some(ix) = ix {
                self.open(ix, cx);
            }
            return;
        }
        if self.showing_archive() {
            let said = "Extract them to open them.".to_string();
            return self.toast(said, false, None, cx);
        }
        // An archive opens here, and only one at a time: in its app it
        // would be extracted outside the owner, with no Undo.
        let zips = chosen.iter().any(|entry| archive::opens_here(entry));
        let files: Vec<PathBuf> = chosen
            .into_iter()
            .filter(|entry| entry.kind != Kind::Dir || workspace::is_package(&entry.path))
            .filter(|entry| !archive::opens_here(entry))
            .map(|entry| entry.path.clone())
            .collect();
        if zips {
            let said = "Archives open one at a time.".to_string();
            self.toast(said, false, None, cx);
        }
        if files.len() <= OPEN_WITHOUT_ASKING {
            for file in files {
                self.open_or_download(file, cx);
            }
            return;
        }
        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &format!("Open {} files?", files.len()),
            Some("Each opens in its app."),
            &["Open", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                let _ = this.update(cx, |this, cx| {
                    for file in files {
                        this.open_or_download(file, cx);
                    }
                });
            }
        })
        .detach();
    }
    /// Opens the file or package `path` in its app, once it's downloaded if
    /// it's a placeholder or holds one. What the listing can't say, such as a
    /// search result's or where a link leads, the disk says off the UI
    /// thread, and a link's target is what downloads.
    pub(in crate::browser) fn open_or_download(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let listed = self.entries.iter().find(|entry| entry.path == path);
        let needs = match listed.map(|entry| (entry.kind, entry.availability)) {
            Some((Kind::Link, _)) | Some((_, icloud::Availability::Unknown)) | None => {
                let asked = path.clone();
                let target = cx.background_executor().spawn(async move {
                    let target = std::fs::canonicalize(&asked).unwrap_or(asked);
                    icloud::needs_download(&target).then_some(target)
                });
                return cx
                    .spawn(async move |this, cx| {
                        let target = target.await;
                        let _ = this.update(cx, |this, cx| match target {
                            Some(target) => this.download_and_open(target, cx),
                            None => this.open_file(&path, cx),
                        });
                    })
                    .detach();
            }
            Some((_, icloud::Availability::Placeholder)) => true,
            _ => self.partly_downloaded.contains(&path),
        };
        match needs {
            true => self.download_and_open(path, cx),
            false => self.open_file(&path, cx),
        }
    }
    pub(in crate::browser) fn open(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(ix) else {
            return;
        };
        let path = entry.path.clone();
        let kind = entry.kind;
        if self.showing_archive() {
            return match kind {
                // Only within the archive asked for: on the way out, or to
                // another archive, the folder would be looked for on the disk.
                Kind::Dir
                    if self.wants_archive()
                        && self
                            .stop()
                            .archive
                            .as_ref()
                            .is_some_and(|archive| path.starts_with(archive)) =>
                {
                    self.navigate(path, cx)
                }
                Kind::Dir => {}
                _ => {
                    let said = format!("Extract {} to open it.", jobs::quoted(&path));
                    self.toast(said, false, None, cx)
                }
            };
        }
        if archive::opens_here(entry) {
            return self.open_archive(path, cx);
        }
        if !matches!(kind, Kind::Dir | Kind::Link) {
            return self.open_or_download(path, cx);
        }
        // Whether it's a package, or where a link leads, is on the disk, maybe
        // a slow volume. A package launches its handler, as in Finder.
        let target = path.clone();
        let goes_in = cx.background_executor().spawn(async move {
            match kind {
                Kind::Dir => !info::is_package(&target),
                _ => target.is_dir() && !info::is_package(&real(&target)),
            }
        });
        let asked_on = self.selected_now();
        self.opening = cx.spawn(async move |this, cx| {
            let goes_in = goes_in.await;
            let _ = this.update(cx, |this, cx| {
                // The selection moved while the disk answered: what was asked
                // for isn't what's wanted now.
                if this.selected_now() != asked_on {
                    return;
                }
                if goes_in {
                    this.navigate(path, cx);
                } else {
                    this.open_or_download(path, cx);
                }
            });
        });
    }
    /// Typing in the list or sidebar searches: the first character moves
    /// focus to the search field and types there. Finder selects by name
    /// instead; search is Monodrive's way in.
    /// E in a preview, or its command: the tools.
    pub(in crate::browser) fn edit_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = &self.preview {
            view.update(cx, |view, cx| view.toggle_edit(window, cx));
        }
    }
    /// Search results and Recents come from anywhere: shows the folder holding
    /// the first selected, with what's selected in it still selected.
    pub(in crate::browser) fn show_in_enclosing_folder(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = self.chosen();
        let Some(folder) = chosen.first().and_then(|path| path.parent()) else {
            return;
        };
        let folder = folder.to_path_buf();
        let select = chosen
            .into_iter()
            .filter(|path| path.parent() == Some(folder.as_path()))
            .collect();
        self.go_to(folder, select, window, cx);
    }
    /// Whether the selection is on this Mac's disk for macOS to share.
    pub(in crate::browser) fn shareable(&self) -> bool {
        !self.selection.is_empty() && !self.showing_remote() && !self.showing_archive()
    }
    /// macOS's share menu on the selection, under `at` in the window.
    /// Share…, with LocalSend among the services while it's on: picking it
    /// opens the LocalSend page holding what was shared.
    pub(in crate::browser) fn share(
        &mut self,
        window: &mut Window,
        at: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let paths = self.chosen();
        let localsend = cx.global::<Preferences>().localsend.then(|| {
            let (pick, picked) = async_channel::bounded(1);
            let held = paths.clone();
            self.localsend.sharing = cx.spawn_in(window, async move |this, cx| {
                if picked.recv().await.is_ok() {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.send_with_localsend(held, window, cx)
                    });
                }
            });
            Box::new(move || {
                let _ = pick.try_send(());
            }) as Box<dyn Fn()>
        });
        crate::sharing::share(&paths, window, at, localsend);
    }
    /// New Folder and New File: an untitled one here, its name ready to
    /// edit.
    pub(in crate::browser) fn new_item(
        &mut self,
        folder: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let here = self.folder().to_path_buf();
        with_owner(
            window,
            cx,
            move |owner| {
                let name = if folder {
                    "untitled folder"
                } else {
                    "untitled"
                };
                let path = disk::free_name(&here.join(name));
                let request = if folder {
                    Request::NewFolder { path }
                } else {
                    Request::NewFile { path }
                };
                let outcome = owner.submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            },
            move |this, outcome, window, cx| this.rename_made(outcome, folder, window, cx),
        );
    }
    /// Makes a folder beside the selection and moves the selection into it,
    /// one change to undo, then edits the folder's name.
    pub(in crate::browser) fn new_folder_with_selection(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let chosen = self.chosen();
        let Some(here) = chosen.first().and_then(|path| path.parent()) else {
            return;
        };
        let here = here.to_path_buf();
        if chosen
            .iter()
            .any(|path| path.parent() != Some(here.as_path()))
        {
            let why = "A new folder can only take items from one folder.".to_string();
            return self.toast(why, true, None, cx);
        }
        with_owner(
            window,
            cx,
            move |owner| {
                let path = disk::free_name(&here.join("New Folder With Items"));
                let request = Request::NewFolderWith {
                    path,
                    items: chosen,
                };
                let outcome = owner.submit(Origin::Window, request.clone())?;
                Ok((request, outcome))
            },
            |this, outcome, window, cx| this.rename_made(outcome, true, window, cx),
        );
    }
    /// Copy: the selected files on the pasteboard, for any app to paste.
    pub(in crate::browser) fn copy_files(&mut self, cx: &mut Context<Self>) {
        let chosen = self.chosen();
        let Some(board) = cx.try_global::<Pasteboard>().filter(|_| !chosen.is_empty()) else {
            return;
        };
        board.write_files(&chosen);
        let what = match chosen.as_slice() {
            [one] => jobs::quoted(one),
            many => format!("{} items", many.len()),
        };
        self.toast(format!("Copied {what} to the clipboard"), false, None, cx);
    }
    /// Paste, or with `moving` Move Here: the files on the pasteboard, from
    /// any app, into this folder as one job, followed until it ends.
    pub(in crate::browser) fn paste(
        &mut self,
        moving: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(board) = cx.try_global::<Pasteboard>() else {
            return;
        };
        let paths = board.files();
        let into = self.folder().to_path_buf();
        self.send(paths, into, moving, window, cx);
    }
    /// Copies or moves `paths` into `into` as one job, followed until it ends.
    pub(in crate::browser) fn send(
        &mut self,
        paths: Vec<PathBuf>,
        into: PathBuf,
        moving: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        // Pasted where they already are, they're duplicated, as in Finder;
        // moved there, nothing changes.
        if paths
            .iter()
            .all(|path| path.parent() == Some(into.as_path()))
        {
            if !moving {
                self.submit(Request::Duplicate { paths }, window, cx);
            }
            return;
        }
        let conflict = Conflict::Stop;
        let request = if moving {
            Request::Move {
                paths,
                into,
                conflict,
            }
        } else {
            Request::Copy {
                paths,
                into,
                conflict,
            }
        };
        with_owner(
            window,
            cx,
            move |owner| owner.submit(Origin::Window, request),
            |this, outcome, _, cx| match outcome {
                Ok(outcome) => cx.emit(PaneEvent::Follow(outcome.op)),
                Err(e) => this.toast(e, true, None, cx),
            },
        );
    }
    /// Copies the selected paths, one per line in the order shown, or the
    /// folder's, ready to paste into an agent's prompt or a terminal.
    pub(in crate::browser) fn copy_path(&mut self, cx: &mut Context<Self>) {
        if let Some(worktree) = self
            .worktrees
            .selected()
            .filter(|_| self.showing_worktrees())
        {
            let text = worktree.display().to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            let said = format!("Copied the path of {}", jobs::quoted(worktree));
            return self.toast(said, false, None, cx);
        }
        let mut paths = self.chosen();
        if paths.is_empty() {
            paths.push(self.disk_folder().to_path_buf());
        }
        let text: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
        let said = match paths.as_slice() {
            [one] => format!("Copied the path of {}", jobs::quoted(one)),
            many => format!("Copied {} paths", many.len()),
        };
        self.toast(said, false, None, cx);
    }
}

impl Pane {
    /// Whether Download would do anything for what's selected.
    pub(in crate::browser) fn can_download(&self, cx: &App) -> bool {
        !self.downloadable(cx).is_empty()
    }

    /// What's selected that Remove Download could act on: downloaded, and in
    /// one of the `cloud` folders, not the folder itself. Whether it's
    /// uploaded is macOS's to say when asked.
    pub(in crate::browser) fn removable(&self, cloud: &[CloudFolder]) -> Vec<PathBuf> {
        self.selection
            .in_order(&self.entries)
            .into_iter()
            .filter(|entry| entry.availability == icloud::Availability::Local)
            .filter(|entry| {
                cloud
                    .iter()
                    .any(|c| entry.path.starts_with(&c.path) && entry.path != c.path)
            })
            .map(|entry| entry.path.clone())
            .collect()
    }

    /// What's selected that isn't all downloaded and isn't on its way.
    pub(in crate::browser) fn downloadable(&self, cx: &App) -> Vec<PathBuf> {
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

    pub(in crate::browser) fn download_selected(&mut self, cx: &mut Context<Self>) {
        let paths = self.downloadable(cx);
        self.download(paths, cx);
    }

    /// Opens the placeholder `path` in its app once it's downloaded, as Finder
    /// does. Handing it over now would leave the app waiting on the network
    /// with nothing on screen saying so.
    pub(in crate::browser) fn download_and_open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // Opening it again while it's on its way opens it once.
        if !self.open_after_download.insert(path.clone()) {
            return;
        }
        let said = format!("Downloading {} to open it", quoted(&path));
        self.toast(said, false, None, cx);
        let then = Self::download_ended(cx);
        self.downloads
            .update(cx, |downloads, cx| downloads.download(vec![path], then, cx));
    }

    /// Asks the provider to keep only placeholders of `paths`, then says what
    /// it did, with Download to bring them back, or why it couldn't.
    pub(in crate::browser) fn remove_downloads_of(
        &mut self,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
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
}

impl Pane {
    /// Hears the app's downloads: repaints for their badges and lists again what
    /// one changed.
    pub(in crate::browser) fn follow_downloads(cx: &mut Context<Pane>) -> Entity<Downloads> {
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

    /// What a pane does when its download ends: opens it if it was opened while
    /// on its way, or says why it failed, with Try Again.
    pub(in crate::browser) fn download_ended(cx: &Context<Pane>) -> Then {
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
}
