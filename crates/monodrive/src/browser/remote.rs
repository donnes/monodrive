//! A folder on a saved SFTP server, which takes the pane's list while it's on
//! screen: listed, sorted and selected as a local folder is, through
//! `monodrive_core::remote`, and never through a local path. A server folder
//! is a stop in the pane's history in front of the local folder it was opened
//! from, so Back leaves it. The connections are the app's (`Places::remote`),
//! and `Servers` says how each is doing, for the sidebar and the palette.

use super::drag::{Spot, Target, offers};
use super::pane::{Pane, PaneEvent, Stop, column, format_date, text_width};
use super::sidebar::Places;
use super::{CancelRename, CommitRename};
use crate::file_icons;
use crate::preferences::Preferences;
use crate::session::{RemoteAt, Shows};
use crate::text_field::TextField;
use crate::theme::{self, ButtonLook, accent, content, danger, icon, on_accent, text_button};
use chrono::Local;
use gpui::{
    AnyElement, App, ClickEvent, ClipboardItem, Context, Focusable, FontWeight, Global,
    MouseButton, ScrollStrategy, Subscription, Task, UniformListScrollHandle, Window, div,
    prelude::*, px, uniform_list,
};
use monodrive_core::entries::{self, Kind};
use monodrive_core::ops::{Conflict, Origin, Request};
use monodrive_core::remote::{
    Connection, Meter, PREVIEW_LIMIT, Preview, Previews, RemoteEntry, RemoteError, RemotePath,
};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// How many entries of one folder the list shows at most. A folder with more
/// says so in the path bar; agents can page through it with remote_list.
pub(super) const MOST_SHOWN: usize = 20_000;

/// How a server's connection is doing, as this app last saw it.
#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Connecting,
    Connected,
    Failed(RemoteError),
}

impl Link {
    /// One line for a sidebar row or the palette.
    pub fn short(&self) -> &'static str {
        match self {
            Link::Connecting => "Connecting…",
            Link::Connected => "Connected",
            Link::Failed(error) => short_reason(error),
        }
    }
}

/// What a failure is called where there's room for a few words.
pub fn short_reason(error: &RemoteError) -> &'static str {
    match error {
        RemoteError::HostKeyChanged(_) => "Host key changed",
        RemoteError::HostKeyUnknown(_) => "Host key not known",
        RemoteError::InteractiveAuth(_) => "Needs a key in ssh-agent",
        RemoteError::AuthFailed(..) => "Authentication failed",
        RemoteError::Unreachable(..) | RemoteError::Timeout(_) => "Didn't answer",
        RemoteError::NoSftp(_) => "No SFTP",
        RemoteError::UnknownSource(_) => "Not saved",
        _ => "Couldn't connect",
    }
}

/// Each saved server's connection in this app, by source id. A server with
/// none isn't connected.
#[derive(Default)]
pub struct Servers(pub HashMap<String, Link>);

impl Global for Servers {}

pub fn link<'a>(id: &str, cx: &'a App) -> Option<&'a Link> {
    cx.try_global::<Servers>()
        .and_then(|servers| servers.0.get(id))
}

fn set_link(id: &str, link: Option<Link>, cx: &mut App) {
    let servers = &mut cx.default_global::<Servers>().0;
    if servers.get(id) == link.as_ref() {
        return;
    }
    match link {
        Some(link) => servers.insert(id.to_owned(), link),
        None => servers.remove(id),
    };
    // Tell whoever draws it; `default_global` alone doesn't.
    cx.update_global::<Servers, _>(|_, _| {});
}

/// The connection to source `id`, opened off the UI thread if it has none,
/// with `Servers` following it.
pub fn reach(id: &str, cx: &mut App) -> Task<Result<Arc<Connection>, RemoteError>> {
    let Some(connections) = cx.try_global::<Places>().map(|p| p.remote.clone()) else {
        return Task::ready(Err(RemoteError::UnknownSource(id.into())));
    };
    if link(id, cx) != Some(&Link::Connected) {
        set_link(id, Some(Link::Connecting), cx);
    }
    let id = id.to_owned();
    let work = {
        let id = id.clone();
        cx.background_executor()
            .spawn(async move { connections.connect(&id) })
    };
    cx.spawn(async move |cx| {
        let result = work.await;
        let link = match &result {
            Ok(_) => Some(Link::Connected),
            Err(RemoteError::UnknownSource(_) | RemoteError::Cancelled) => None,
            Err(error) => Some(Link::Failed(error.clone())),
        };
        cx.update(|cx| set_link(&id, link, cx));
        result
    })
}

/// Ends the app's connection to `id`. Nothing on the server changes, and a
/// copy the owner is making keeps its own connection.
pub fn disconnect(id: &str, cx: &mut App) {
    if let Some(connections) = cx.try_global::<Places>().map(|p| p.remote.clone()) {
        let id = id.to_owned();
        cx.background_executor()
            .spawn(async move { connections.disconnect(&id) })
            .detach();
    }
    set_link(id, None, cx);
}

/// Where a server folder's listing is.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) enum Listing {
    #[default]
    Connecting,
    Listed,
    Failed(RemoteError),
    /// Disconnect ended it; nothing reconnects until asked.
    Disconnected,
}

/// What the pane shows of a server folder, as last listed.
pub(in crate::browser) struct RemotePage {
    /// The folder listed, once the server has said where its start is.
    pub(super) at: Option<RemotePath>,
    pub(super) listing: Listing,
    pub(super) entries: Vec<RemoteEntry>,
    /// The folder holds more than `MOST_SHOWN`.
    more: bool,
    /// Names the server sent that Monodrive can't address.
    left_out: u64,
    /// Selected entries, by name: they're all in one folder.
    selected: HashSet<String>,
    /// The entry the keys are on, and where ⇧ extends from.
    cursor: Option<usize>,
    anchor: Option<usize>,
    /// What to select once the folder lists: the one just left, or one just
    /// made or renamed.
    select_after: Option<String>,
    pub(super) renaming: Option<RemoteRenaming>,
    scroll: UniformListScrollHandle,
    loading: Task<()>,
}

pub(super) struct RemoteRenaming {
    /// The entry's name as it was.
    name: String,
    field: gpui::Entity<TextField>,
    pub(super) error: Option<String>,
    sending: bool,
    _blur: Subscription,
}

impl Default for RemotePage {
    fn default() -> Self {
        Self {
            at: None,
            listing: Listing::default(),
            entries: Vec::new(),
            more: false,
            left_out: 0,
            selected: HashSet::new(),
            cursor: None,
            anchor: None,
            select_after: None,
            renaming: None,
            scroll: UniformListScrollHandle::new(),
            loading: Task::ready(()),
        }
    }
}

impl RemotePage {
    pub(super) fn leave(&mut self) {
        self.loading = Task::ready(());
        self.renaming = None;
    }

    fn chosen(&self) -> Vec<&RemoteEntry> {
        self.entries
            .iter()
            .filter(|entry| self.selected.contains(&entry.name))
            .collect()
    }

    fn select_only(&mut self, ix: usize) {
        self.selected.clear();
        if let Some(entry) = self.entries.get(ix) {
            self.selected.insert(entry.name.clone());
            self.cursor = Some(ix);
            self.anchor = Some(ix);
        }
    }

    fn extend_to(&mut self, ix: usize) {
        // A listing that shrank may have taken the anchor's row.
        let anchor = self
            .anchor
            .filter(|anchor| *anchor < self.entries.len())
            .unwrap_or(ix);
        self.anchor = Some(anchor);
        self.selected = self.entries[anchor.min(ix)..=anchor.max(ix)]
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        self.cursor = Some(ix);
    }
}

impl Stop {
    /// A server folder, shown in front of the local `folder`.
    pub(super) fn remote(folder: PathBuf, at: RemoteAt) -> Self {
        let mut stop = Stop::folder(folder);
        stop.shows = Shows::Remote;
        stop.remote = Some(at);
        stop
    }
}

impl Pane {
    /// Whether a server folder is on screen: a search in front of it isn't.
    pub(super) fn showing_remote(&self) -> bool {
        self.query.is_empty() && self.stop().shows == Shows::Remote
    }

    /// The server the pane shows, by source id.
    pub(super) fn remote_source(&self) -> Option<&str> {
        self.showing_remote()
            .then(|| self.stop().remote.as_ref().map(RemoteAt::source))
            .flatten()
    }

    /// Opens server `source` where it starts, connecting if it has to.
    pub(super) fn open_server(
        &mut self,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let here = RemoteAt::Start(source.to_owned());
        if self.remote_source() != Some(source) || self.stop().remote.as_ref() != Some(&here) {
            self.push_stop(Stop::remote(self.disk_folder().to_path_buf(), here), cx);
        } else {
            self.show_folder(cx);
        }
        self.focus_list(window, cx);
    }

    /// Goes into a folder on the server on screen.
    fn go_remote(&mut self, at: RemotePath, cx: &mut Context<Self>) {
        self.remote.select_after = None;
        self.push_stop(
            Stop::remote(self.disk_folder().to_path_buf(), RemoteAt::At(at)),
            cx,
        );
    }

    /// ⌘↑: the folder this one is in, selecting this one.
    fn remote_parent(&mut self, cx: &mut Context<Self>) {
        let Some(at) = self.remote.at.clone() else {
            return;
        };
        let Some(parent) = at.parent() else {
            return;
        };
        let left = at.name().map(str::to_owned);
        self.go_remote(parent, cx);
        self.remote.select_after = left;
    }

    /// Lists the server folder on screen: connects, then reads this folder
    /// alone, off the UI thread. `quiet` keeps what's on screen meanwhile.
    pub(super) fn list_remote(&mut self, quiet: bool, cx: &mut Context<Self>) {
        let Some(at) = self.stop().remote.clone() else {
            return;
        };
        let known = match &at {
            RemoteAt::At(path) => Some(path),
            RemoteAt::Start(_) => None,
        };
        if !quiet || known != self.remote.at.as_ref() {
            self.remote.entries.clear();
            self.remote.selected.clear();
            self.remote.cursor = None;
            self.remote.anchor = None;
            self.remote.more = false;
            self.remote.left_out = 0;
            self.remote.renaming = None;
            self.remote.at = known.cloned();
            self.remote.listing = Listing::Connecting;
            self.remote.scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        let reach = reach(at.source(), cx);
        let sort = self.sort;
        let show_hidden = cx.global::<Preferences>().show_hidden;
        self.remote.loading = cx.spawn(async move |this, cx| {
            let listed = match reach.await {
                Ok(open) => {
                    cx.background_executor()
                        .spawn(async move {
                            let path = match at {
                                RemoteAt::Start(_) => open.start()?,
                                RemoteAt::At(path) => path,
                            };
                            let page = open.list(&path, 0, MOST_SHOWN)?;
                            Ok((path, page))
                        })
                        .await
                }
                Err(error) => Err(error),
            };
            let _ = this.update(cx, |this, cx| {
                let page = &mut this.remote;
                match listed {
                    Ok((path, listed)) => {
                        // Where the server starts is known now, for Back too.
                        let stop = &mut this.history[this.at];
                        if matches!(stop.remote, Some(RemoteAt::Start(_))) {
                            stop.remote = Some(RemoteAt::At(path.clone()));
                        }
                        let mut entries = listed.entries;
                        if !show_hidden {
                            entries.retain(|entry| !entry.name.starts_with('.'));
                        }
                        entries::sort(&mut entries, sort);
                        page.entries = entries;
                        page.more = listed.more;
                        page.left_out = listed.left_out;
                        page.at = Some(path);
                        page.listing = Listing::Listed;
                        let names: HashSet<&str> =
                            page.entries.iter().map(|e| e.name.as_str()).collect();
                        page.selected.retain(|name| names.contains(name.as_str()));
                        if let Some(name) = page.select_after.take()
                            && let Some(ix) = page.entries.iter().position(|e| e.name == name)
                        {
                            page.select_only(ix);
                            page.scroll.scroll_to_item(ix, ScrollStrategy::Center);
                        }
                        page.cursor =
                            page.cursor
                                .filter(|ix| *ix < page.entries.len())
                                .or_else(|| {
                                    page.entries
                                        .iter()
                                        .position(|e| page.selected.contains(&e.name))
                                });
                    }
                    Err(RemoteError::Cancelled) => {}
                    Err(error) => {
                        page.entries.clear();
                        page.selected.clear();
                        page.listing = Listing::Failed(error);
                    }
                }
                cx.emit(PaneEvent::Moved);
                cx.notify();
            });
        });
    }

    /// A new sort orders what's listed, without asking the server again.
    pub(super) fn resort_remote(&mut self, cx: &mut Context<Self>) {
        let page = &mut self.remote;
        let cursor = page
            .cursor
            .and_then(|ix| page.entries.get(ix))
            .map(|e| e.name.clone());
        entries::sort(&mut page.entries, self.sort);
        page.cursor = cursor.and_then(|name| page.entries.iter().position(|e| e.name == name));
        page.anchor = page.cursor;
        cx.notify();
    }

    /// Disconnect ended source `id`: a folder of it on screen says so and
    /// waits, rather than connecting again by itself.
    pub(super) fn remote_disconnected(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.remote_source() == Some(id) {
            self.remote.leave();
            self.remote.listing = Listing::Disconnected;
            self.remote.entries.clear();
            self.remote.selected.clear();
            cx.notify();
        }
    }

    /// The selected items on the server, in the order shown.
    pub(super) fn remote_chosen(&self) -> Vec<RemotePath> {
        self.remote
            .chosen()
            .into_iter()
            .map(|entry| entry.path.clone())
            .collect()
    }

    pub(super) fn remote_renaming(&self) -> bool {
        self.showing_remote() && self.remote.renaming.is_some()
    }

    pub(super) fn remote_has_selection(&self) -> bool {
        self.showing_remote() && !self.remote.selected.is_empty()
    }

    /// The server folder on screen, if it's listed and takes new items.
    pub(super) fn remote_folder(&self) -> Option<&RemotePath> {
        (self.showing_remote() && self.remote.listing == Listing::Listed)
            .then_some(self.remote.at.as_ref())
            .flatten()
    }

    /// What the top bar calls the server folder: its name, or the server's at
    /// its root and before its start is known.
    pub(super) fn remote_title(&self) -> String {
        match (&self.remote.at, self.stop().remote.as_ref()) {
            (Some(at), _) => at.name().unwrap_or(at.source()).to_owned(),
            (None, Some(at)) => at.source().to_owned(),
            (None, None) => String::new(),
        }
    }

    fn remote_step(&mut self, by: isize, extend: bool, cx: &mut Context<Self>) {
        let count = self.remote.entries.len();
        if count == 0 {
            return;
        }
        let ix = match self.remote.cursor {
            None if by < 0 => count - 1,
            None => 0,
            Some(ix) => ix.saturating_add_signed(by).min(count - 1),
        };
        self.remote_select_ix(ix, extend, cx);
    }

    fn remote_select_ix(&mut self, ix: usize, extend: bool, cx: &mut Context<Self>) {
        if extend {
            self.remote.extend_to(ix);
        } else {
            self.remote.select_only(ix);
        }
        self.remote
            .scroll
            .scroll_to_item(ix, ScrollStrategy::Nearest);
        cx.notify();
    }

    fn remote_click(
        &mut self,
        ix: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(name) = self.remote.entries.get(ix).map(|e| e.name.clone()) else {
            return;
        };
        self.focus_list(window, cx);
        let modifiers = event.modifiers();
        if modifiers.platform {
            if !self.remote.selected.remove(&name) {
                self.remote.selected.insert(name);
            }
            self.remote.cursor = Some(ix);
            self.remote.anchor = Some(ix);
        } else if modifiers.shift {
            self.remote.extend_to(ix);
        } else {
            self.remote.select_only(ix);
            if event.click_count() == 2 {
                self.remote_open(false, cx);
            }
        }
        cx.notify();
    }

    /// Open: a folder goes in, and a file comes down to the preview cache
    /// to open in its app, or to Quick Look with `look`.
    pub(super) fn remote_open(&mut self, look: bool, cx: &mut Context<Self>) {
        let chosen: Vec<RemoteEntry> = self.remote.chosen().into_iter().cloned().collect();
        let [only] = chosen.as_slice() else {
            let files: Vec<(RemotePath, u64)> = chosen
                .iter()
                .filter(|entry| entry.kind != Kind::Dir)
                .map(|entry| (entry.path.clone(), entry.size))
                .collect();
            if let Some((at, size)) = files.into_iter().next() {
                cx.emit(PaneEvent::Fetch { at, size, look });
            }
            return;
        };
        match only.kind {
            Kind::Dir if !look => self.go_remote(only.path.clone(), cx),
            Kind::Dir => {}
            // A link may lead to a folder; the server says which.
            Kind::Link => {
                let (Some(source), path) = (self.remote_source(), only.path.clone()) else {
                    return;
                };
                let reach = reach(source, cx);
                self.remote.loading = cx.spawn(async move |this, cx| {
                    let followed = match reach.await {
                        Ok(open) => {
                            let at = path.clone();
                            cx.background_executor()
                                .spawn(async move { open.follow(&at) })
                                .await
                        }
                        Err(error) => Err(error),
                    };
                    let _ = this.update(cx, |this, cx| match followed {
                        Ok(entry) if entry.kind == Kind::Dir && !look => this.go_remote(path, cx),
                        Ok(entry) if entry.kind == Kind::Dir => {}
                        Ok(entry) => cx.emit(PaneEvent::Fetch {
                            at: path,
                            size: entry.size,
                            look,
                        }),
                        Err(error) => this.toast(error.to_string(), true, None, cx),
                    });
                });
            }
            Kind::File | Kind::Other => cx.emit(PaneEvent::Fetch {
                at: only.path.clone(),
                size: only.size,
                look,
            }),
        }
    }

    /// Copy Path: `source:/path` a line, as agents name them.
    fn remote_copy_path(&mut self, cx: &mut Context<Self>) {
        let mut paths = self.remote_chosen();
        if paths.is_empty() {
            paths.extend(self.remote.at.clone());
        }
        let said = match paths.as_slice() {
            [] => return,
            [one] => format!(
                "Copied the path of \"{}\"",
                one.name().unwrap_or(one.source())
            ),
            many => format!("Copied {} paths", many.len()),
        };
        let text: Vec<String> = paths.iter().map(ToString::to_string).collect();
        cx.write_to_clipboard(ClipboardItem::new_string(text.join("\n")));
        self.toast(said, false, None, cx);
    }

    /// Runs `change` on the server's connection off the UI thread, then lists
    /// the folder again, quietly, selecting `select`.
    fn remote_change(
        &mut self,
        select: Option<String>,
        change: impl FnOnce(&Connection) -> Result<(), RemoteError> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<(), RemoteError>, &mut Window, &mut Context<Self>) + 'static,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = self.remote_source() else {
            return;
        };
        let reach = reach(source, cx);
        cx.spawn_in(window, async move |this, cx| {
            let changed = match reach.await {
                Ok(open) => {
                    cx.background_executor()
                        .spawn(async move { change(&open) })
                        .await
                }
                Err(error) => Err(error),
            };
            let _ = this.update_in(cx, |this, window, cx| {
                done(this, changed.clone(), window, cx);
                if changed.is_ok() {
                    this.remote.select_after = select;
                    this.list_remote(true, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// New Folder: makes "untitled folder", numbered if that's taken, and
    /// edits its name.
    fn remote_new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(at) = self.remote_folder().cloned() else {
            return;
        };
        let taken: HashSet<&str> = self
            .remote
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect();
        let name = std::iter::once("untitled folder".to_string())
            .chain((2..).map(|n| format!("untitled folder {n}")))
            .find(|name| !taken.contains(name.as_str()))
            .unwrap_or_default();
        let Ok(path) = at.join(&name) else {
            return;
        };
        let made = path.clone();
        self.remote_change(
            Some(name.clone()),
            move |open| open.mkdir(&path),
            move |this, done, window, cx| match done {
                // It's there, so its row is too, before the listing says so.
                Ok(()) => {
                    let page = &mut this.remote;
                    page.entries.push(RemoteEntry {
                        path: made,
                        name: name.clone(),
                        kind: Kind::Dir,
                        size: 0,
                        modified: 0,
                        permissions: None,
                        link: None,
                    });
                    entries::sort(&mut page.entries, this.sort);
                    if let Some(ix) = page.entries.iter().position(|e| e.name == name) {
                        page.select_only(ix);
                        page.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
                    }
                    this.remote_start_rename(name, window, cx);
                }
                Err(error) => this.toast(error.to_string(), true, None, cx),
            },
            window,
            cx,
        );
    }

    /// ↩ on one item: edits its name in place.
    fn remote_start_rename(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let folder = self
            .remote
            .entries
            .iter()
            .any(|e| e.name == name && e.kind == Kind::Dir);
        let stem = name
            .rfind('.')
            .filter(|dot| *dot > 0 && !folder)
            .unwrap_or(name.len());
        let size = cx.global::<Preferences>().row_size.metrics().2;
        let field = cx.new(|cx| {
            let mut field = TextField::new("Name", "", cx).with_font_size(size);
            field.set_text(&name, 0..stem, cx);
            field
        });
        let focus = field.focus_handle(cx);
        let blur = cx.on_blur(&focus, window, |this, window, cx| {
            this.remote_commit_rename(window, cx)
        });
        window.focus(&focus, cx);
        self.remote.renaming = Some(RemoteRenaming {
            name,
            field,
            error: None,
            sending: false,
            _blur: blur,
        });
        cx.notify();
    }

    /// ↩ or clicking away: renames on the server, never over another item.
    fn remote_commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(renaming) = self.remote.renaming.as_mut().filter(|r| !r.sending) else {
            return;
        };
        let new = renaming.field.read(cx).text().to_string();
        let old = renaming.name.clone();
        let Some(at) = self.remote.at.clone() else {
            return;
        };
        if new == old {
            return self.remote_end_rename(window, cx);
        }
        let paths = at.join(&old).and_then(|from| Ok((from, at.join(&new)?)));
        let (from, to) = match paths {
            Ok(paths) => paths,
            Err(_) => {
                renaming.error = Some(format!("\"{new}\" can't be a name."));
                return cx.notify();
            }
        };
        renaming.sending = true;
        self.remote_change(
            Some(new),
            move |open| open.rename(&from, &to),
            move |this, done, window, cx| {
                let still = this.remote.renaming.as_mut().filter(|r| r.name == old);
                match (done, still) {
                    (Ok(()), _) => this.remote_end_rename(window, cx),
                    (Err(error), Some(renaming)) => {
                        renaming.sending = false;
                        renaming.error = Some(error.to_string());
                    }
                    (Err(error), None) => this.toast(error.to_string(), true, None, cx),
                }
            },
            window,
            cx,
        );
    }

    fn remote_end_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(renaming) = self.remote.renaming.take()
            && renaming.field.focus_handle(cx).is_focused(window)
        {
            self.focus_list(window, cx);
        }
        cx.notify();
    }

    /// Copy to Other Pane from a server folder: a download into `into`.
    pub(super) fn remote_download(
        &mut self,
        into: PathBuf,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let paths = self.remote_chosen();
        self.download_from_server(paths, into, window, cx);
    }

    /// What a drag from server row `ix` carries, and the row's kind and name
    /// to draw. A row outside the selection joins it as a click with `keys`
    /// would: ⌘ adds it, ⇧ extends to it, and otherwise it's alone.
    pub(super) fn remote_drag_from(
        &mut self,
        ix: usize,
        keys: gpui::Modifiers,
    ) -> (Vec<RemotePath>, Option<(Kind, String)>) {
        let Some(entry) = self.remote.entries.get(ix) else {
            return (Vec::new(), None);
        };
        let picture = Some((entry.kind, entry.name.clone()));
        if !self.remote.selected.contains(&entry.name) {
            if keys.platform {
                self.remote.selected.insert(entry.name.clone());
                self.remote.cursor = Some(ix);
                self.remote.anchor = Some(ix);
            } else if keys.shift {
                self.remote.extend_to(ix);
            } else {
                self.remote.select_only(ix);
            }
        }
        (self.remote_chosen(), picture)
    }

    /// Copies `paths` on a server down into `into`, as a job.
    pub(super) fn download_from_server(
        &mut self,
        paths: Vec<RemotePath>,
        into: PathBuf,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.queue_transfer(
            Request::Download {
                paths,
                into,
                conflict: Conflict::Stop,
            },
            window,
            cx,
        );
    }

    /// Copy to Other Pane into a server folder: an upload of `paths`.
    pub(super) fn upload(
        &mut self,
        paths: Vec<PathBuf>,
        into: RemotePath,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        self.queue_transfer(
            Request::Upload {
                paths,
                into,
                conflict: Conflict::Stop,
            },
            window,
            cx,
        );
    }

    fn queue_transfer(&mut self, request: Request, window: &Window, cx: &mut Context<Self>) {
        super::with_owner(
            window,
            cx,
            move |owner| owner.submit(Origin::Window, request),
            |this, outcome, _, cx| match outcome {
                Ok(outcome) => cx.emit(PaneEvent::Follow(outcome.op)),
                Err(e) => this.toast(e, true, None, cx),
            },
        );
    }

    /// The keys and menu commands that act on a server folder, in place of a
    /// local folder's: what SFTP can't do has no handler, so its menu item
    /// is off.
    pub(super) fn remote_commands(
        &mut self,
        root: gpui::Div,
        sheet: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        use super::{
            Back, CopyPath, DeselectAll, EnclosingFolder, Forward, GoApplications, GoComputer,
            GoDesktop, GoDocuments, GoDownloads, GoHome, GoOverview, GoRecents, GoTrash, NewFolder,
            OpenSelected, Rename, SelectFirst, SelectLast, SelectNext, SelectPrevious,
            ShowContextMenu, home,
        };
        let selected = !self.remote.selected.is_empty();
        let renaming = self.remote.renaming.is_some();
        let listed = self.remote_folder().is_some();
        root.when(!sheet, |d| {
            d.on_action(cx.listener(|this, _: &Back, _, cx| this.step_history(-1, cx)))
                .on_action(cx.listener(|this, _: &Forward, _, cx| this.step_history(1, cx)))
                .on_action(cx.listener(|this, _: &EnclosingFolder, _, cx| this.remote_parent(cx)))
                .on_action(
                    cx.listener(|this, _: &SelectPrevious, _, cx| this.remote_step(-1, false, cx)),
                )
                .on_action(
                    cx.listener(|this, _: &SelectNext, _, cx| this.remote_step(1, false, cx)),
                )
                .on_action(cx.listener(|this, _: &SelectFirst, _, cx| {
                    if !this.remote.entries.is_empty() {
                        this.remote_select_ix(0, false, cx)
                    }
                }))
                .on_action(cx.listener(|this, _: &SelectLast, _, cx| {
                    if let Some(last) = this.remote.entries.len().checked_sub(1) {
                        this.remote_select_ix(last, false, cx)
                    }
                }))
                .on_action(cx.listener(|this, _: &GoHome, window, cx| this.go(home(), window, cx)))
                .on_action(
                    cx.listener(|this, _: &GoRecents, window, cx| this.open_recents(window, cx)),
                )
                .on_action(cx.listener(|this, _: &GoOverview, window, cx| {
                    this.open_page(Shows::Overview, window, cx)
                }))
                .on_action(cx.listener(|this, _: &GoTrash, window, cx| this.open_trash(window, cx)))
                .on_action(cx.listener(|this, _: &GoDesktop, window, cx| {
                    this.go(home().join("Desktop"), window, cx)
                }))
                .on_action(cx.listener(|this, _: &GoDocuments, window, cx| {
                    this.go(home().join("Documents"), window, cx)
                }))
                .on_action(cx.listener(|this, _: &GoDownloads, window, cx| {
                    this.go(home().join("Downloads"), window, cx)
                }))
                .on_action(cx.listener(|this, _: &GoApplications, window, cx| {
                    this.go("/Applications".into(), window, cx)
                }))
                .on_action(
                    cx.listener(|this, _: &GoComputer, window, cx| this.go("/".into(), window, cx)),
                )
                .on_action(cx.listener(|this, _: &DeselectAll, _, cx| {
                    this.remote.selected.clear();
                    cx.notify();
                }))
                .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.remote_copy_path(cx)))
                .when(selected && !renaming, |d| {
                    d.on_action(
                        cx.listener(|this, _: &OpenSelected, _, cx| this.remote_open(false, cx)),
                    )
                })
                .when(listed && !renaming, |d| {
                    d.on_action(cx.listener(|this, _: &NewFolder, window, cx| {
                        this.remote_new_folder(window, cx)
                    }))
                })
                .when(self.remote.selected.len() == 1 && !renaming, |d| {
                    d.on_action(cx.listener(|this, _: &Rename, window, cx| {
                        if let Some(name) = this.remote.selected.iter().next().cloned() {
                            this.remote_start_rename(name, window, cx)
                        }
                    }))
                })
                .on_action(cx.listener(|this, _: &ShowContextMenu, _, cx| {
                    if this.remote.renaming.is_none() {
                        let at = this.menu_point(cx);
                        let scopes = this.remote_scopes();
                        cx.emit(PaneEvent::Menu { scopes, at });
                    }
                }))
        })
    }

    pub(super) fn remote_scopes(&self) -> Vec<crate::commands::Scope> {
        use crate::commands::Scope;
        match self.remote.selected.len() {
            0 => vec![Scope::Folder],
            1 => vec![Scope::Selection],
            _ => vec![Scope::Selection, Scope::Several],
        }
    }

    /// The server folder in place of the list: its rows, or why there are
    /// none.
    pub(super) fn remote_list(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use super::{ExtendNext, ExtendPrevious, PageDown, PageUp, ScrollToBottom, ScrollToTop};
        let body = match &self.remote.listing {
            Listing::Listed if self.remote.entries.is_empty() => {
                remote_note("This folder is empty.").into_any_element()
            }
            Listing::Listed => uniform_list(
                "remote-entries",
                self.remote.entries.len(),
                cx.processor(Self::remote_rows),
            )
            .track_scroll(&self.remote.scroll)
            .flex_1()
            .p_1()
            .into_any_element(),
            Listing::Connecting => {
                let source = self
                    .stop()
                    .remote
                    .as_ref()
                    .map(|at| at.source().to_owned())
                    .unwrap_or_default();
                remote_note(format!("Connecting to {source}…")).into_any_element()
            }
            Listing::Failed(error) => self
                .remote_failed(error.to_string(), true, cx)
                .into_any_element(),
            Listing::Disconnected => self
                .remote_failed("Disconnected.".to_string(), false, cx)
                .into_any_element(),
        };
        let _ = window;
        let drops = self
            .drag
            .active()
            .then(|| self.remote_folder().cloned())
            .flatten();
        let pane = cx.entity();
        div()
            .id("remote-list")
            .when_some(drops, |d, at| {
                let drag = self.drag.clone();
                let name = on_server(&at);
                let spot = Spot::Pane(pane.entity_id());
                offers(d, move |_| {
                    drag.offer(Target::Server {
                        spot: spot.clone(),
                        at: at.clone(),
                        name: name.clone(),
                    })
                })
            })
            .key_context("FileList")
            .track_focus(&self.list_focus)
            .on_action(cx.listener(|this, _: &ScrollToTop, _, cx| {
                this.remote.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ScrollToBottom, _, cx| {
                if let Some(last) = this.remote.entries.len().checked_sub(1) {
                    this.remote
                        .scroll
                        .scroll_to_item(last, ScrollStrategy::Bottom);
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.remote_step(-20, false, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.remote_step(20, false, cx)))
            .on_action(
                cx.listener(|this, _: &ExtendPrevious, _, cx| this.remote_step(-1, true, cx)),
            )
            .on_action(cx.listener(|this, _: &ExtendNext, _, cx| this.remote_step(1, true, cx)))
            .on_action(
                cx.listener(|this, _: &crate::text_field::SelectAll, _, cx| {
                    let page = &mut this.remote;
                    page.selected = page.entries.iter().map(|e| e.name.clone()).collect();
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| this.focus_list(window, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    window.prevent_default();
                    this.focus_list(window, cx);
                    let scopes = this.remote_scopes();
                    cx.emit(PaneEvent::Menu {
                        scopes,
                        at: event.position,
                    });
                }),
            )
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(body)
            .into_any_element()
    }

    /// Why a server folder can't show: the server's name, the reason, in red
    /// if it `failed`, then Back and Try Again.
    fn remote_failed(&self, why: String, failed: bool, cx: &mut Context<Self>) -> gpui::Div {
        let title = self.remote_title();
        let back = self.at > 0;
        div()
            .debug_selector(|| "remote-failed".into())
            .flex_1()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .child(
                div()
                    .text_size(px(theme::TEXT_TITLE))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title),
            )
            .child(
                div()
                    .max_w(px(420.))
                    .text_center()
                    .text_size(px(theme::TEXT_BODY))
                    .text_color(if failed { danger() } else { content(0.65) })
                    .child(why),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .gap_2()
                    .child(
                        text_button(
                            "remote-back",
                            "Back",
                            if back {
                                ButtonLook::Secondary
                            } else {
                                ButtonLook::Disabled
                            },
                        )
                        .when(back, |d| {
                            d.on_click(cx.listener(|this, _, _, cx| this.step_history(-1, cx)))
                        }),
                    )
                    .child(
                        text_button("remote-retry", "Try Again", ButtonLook::Primary)
                            .debug_selector(|| "remote-retry".into())
                            .on_click(cx.listener(|this, _, _, cx| this.list_remote(false, cx))),
                    ),
            )
    }

    /// The rows of a server folder, as a local folder's list draws them.
    fn remote_rows(
        &mut self,
        range: std::ops::Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let today = Local::now().date_naive();
        let (date, size, kind) = super::pane::columns(self.chrome.width);
        let (row_height, icon_size, text_size) = cx.global::<Preferences>().row_size.metrics();
        let active = self.list_focus.contains_focused(window, cx) || self.chrome.menu_here;
        let drops = self.drag.active();
        let id = cx.entity_id();
        range
            .filter_map(|ix| {
                let entry = self.remote.entries.get(ix)?;
                let selected = self.remote.selected.contains(&entry.name);
                let ink = if selected && active {
                    on_accent()
                } else if selected {
                    content(1.)
                } else {
                    content(0.65)
                };
                let field = self
                    .remote
                    .renaming
                    .as_ref()
                    .filter(|r| r.name == entry.name)
                    .map(|renaming| remote_name_field(renaming, cx));
                Some(
                    div()
                        .id(("remote-row", ix))
                        .debug_selector(move || format!("remote-row-{ix}"))
                        .role(gpui::Role::ListItem)
                        .aria_label(entry.name.clone())
                        .aria_selected(selected)
                        .w_full()
                        .h(px(row_height))
                        .px_3()
                        .rounded(px(theme::RADIUS_ROW))
                        .flex()
                        .items_center()
                        .gap_3()
                        .text_size(px(text_size))
                        .map(|d| {
                            if selected {
                                d.bg(if active { accent() } else { content(0.14) })
                                    .text_color(if active { on_accent() } else { content(1.) })
                            } else if ix % 2 == 1 {
                                d.bg(content(0.03))
                            } else {
                                d
                            }
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(file_icons::kind_icon(
                                    entry.kind,
                                    Path::new(&entry.name),
                                    icon_size,
                                    window,
                                    cx,
                                ))
                                .child(match field {
                                    Some(field) => field.ml(px(-7.)).into_any_element(),
                                    None => div()
                                        .min_w_0()
                                        .truncate()
                                        .child(entry.name.clone())
                                        .into_any_element(),
                                }),
                        )
                        .when(date, |d| {
                            d.child(column(180., false).text_color(ink).child(
                                if entry.modified == 0 {
                                    "--".to_string()
                                } else {
                                    format_date(entry.modified, today)
                                },
                            ))
                        })
                        .when(size, |d| {
                            d.child(column(80., true).text_color(ink).child(
                                if entry.kind == Kind::File {
                                    super::format_bytes(entry.size)
                                } else {
                                    "--".to_string()
                                },
                            ))
                        })
                        .when(kind, |d| {
                            d.child(
                                column(110., false)
                                    .text_color(ink)
                                    .child(entries::Sorted::kind_label(entry)),
                            )
                        })
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.remote_click(ix, event, window, cx)
                        }))
                        .when(self.remote.renaming.is_none(), |d| {
                            Self::grab_from_server(d, ix, cx)
                        })
                        .when(drops && entry.kind == Kind::Dir, |d| {
                            let drag = self.drag.clone();
                            let target = Target::Server {
                                spot: Spot::Row(id, ix),
                                at: entry.path.clone(),
                                name: on_server(&entry.path),
                            };
                            offers(d, move |_| drag.offer(target.clone()))
                        })
                        .when(self.drag.outlines(&Spot::Row(id, ix)), |d| {
                            d.relative().child(
                                self.drag
                                    .pulsed(theme::drop_outline().rounded(px(theme::RADIUS_ROW))),
                            )
                        })
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                cx.stop_propagation();
                                window.prevent_default();
                                if let Some(name) = this.remote.entries.get(ix).map(|e| &e.name)
                                    && !this.remote.selected.contains(name)
                                {
                                    this.remote.select_only(ix);
                                }
                                this.focus_list(window, cx);
                                let scopes = this.remote_scopes();
                                cx.emit(PaneEvent::Menu {
                                    scopes,
                                    at: event.position,
                                });
                            }),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }

    /// The path bar's crumbs for a server folder: the server, then each
    /// folder down to this one. A press on one goes there.
    pub(super) fn remote_crumbs(
        &self,
        room: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let Some(at) = self.remote.at.clone().or_else(|| {
            self.stop()
                .remote
                .as_ref()
                .and_then(|at| RemotePath::new(at.source(), "/").ok())
        }) else {
            return div();
        };
        let mut steps = vec![at.clone()];
        while let Some(parent) = steps.last().and_then(RemotePath::parent) {
            steps.push(parent);
        }
        steps.reverse();
        let last = steps.len() - 1;
        let name = |ix: usize, step: &RemotePath| {
            if ix == 0 {
                step.source().to_owned()
            } else {
                step.name().unwrap_or_default().to_owned()
            }
        };
        // Levels that don't fit go from the left, as a local path's do.
        let widths: Vec<f32> = steps
            .iter()
            .enumerate()
            .map(|(ix, step)| {
                text_width(
                    &name(ix, step),
                    theme::TEXT_SMALL,
                    FontWeight::NORMAL,
                    window,
                ) + 22.
            })
            .collect();
        let hidden = super::pane::hidden_levels(&widths, 20., room);
        div()
            .flex_shrink_0()
            .min_w_0()
            .flex()
            .items_center()
            .gap(px(1.))
            .overflow_hidden()
            .when(hidden > 0, |d| {
                d.child(
                    div()
                        .px(px(theme::CRUMB_INSET))
                        .text_color(content(0.65))
                        .child("…"),
                )
            })
            .children(
                steps
                    .into_iter()
                    .enumerate()
                    .skip(hidden)
                    .map(|(ix, step)| {
                        let here = ix == last;
                        let label = name(ix, &step);
                        div()
                            .flex()
                            .items_center()
                            .gap(px(1.))
                            .min_w_0()
                            .when(ix > hidden, |d| {
                                d.child(icon("chevron-right", 12.).text_color(content(0.3)))
                            })
                            .child(
                                div()
                                    .id(("remote-step", ix))
                                    .debug_selector(move || format!("remote-crumb-{ix}"))
                                    .role(gpui::Role::Link)
                                    .aria_label(label.clone())
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .px(px(theme::CRUMB_INSET))
                                    .rounded_full()
                                    .line_height(px(18.))
                                    .when(ix == 0, |d| {
                                        d.child(icon("server", 12.).text_color(content(0.55)))
                                    })
                                    .child(div().truncate().child(label))
                                    .map(|d| {
                                        if here {
                                            d.font_weight(FontWeight::SEMIBOLD)
                                        } else {
                                            d.text_color(content(0.65))
                                                .cursor_pointer()
                                                .hover(|s| {
                                                    s.bg(content(0.08)).text_color(content(1.))
                                                })
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.go_remote(step.clone(), cx)
                                                }))
                                        }
                                    }),
                            )
                    }),
            )
    }

    /// The path bar's status for a server folder: as a local folder's, then
    /// which server it's on.
    pub(super) fn remote_status(&self) -> String {
        let page = &self.remote;
        let count = page.entries.len();
        let status = match page.selected.len() {
            0 if count == 1 => "1 item".to_string(),
            0 => format!("{count} items"),
            chosen => {
                let size: u64 = page
                    .chosen()
                    .iter()
                    .filter(|e| e.kind == Kind::File)
                    .map(|e| e.size)
                    .sum();
                if size == 0 {
                    format!("{chosen} of {count} selected")
                } else {
                    format!(
                        "{chosen} of {count} selected, {}",
                        super::format_bytes(size)
                    )
                }
            }
        };
        let status = if page.more {
            format!("{status}, more not shown")
        } else {
            status
        };
        let status = match page.left_out {
            0 => status,
            n => format!("{status}, {n} unreadable names not shown"),
        };
        match self.remote_source() {
            Some(source) if page.listing == Listing::Listed => format!("{status} · on {source}"),
            _ => String::new(),
        }
    }
}

fn remote_note(text: impl Into<gpui::SharedString>) -> gpui::Div {
    div()
        .debug_selector(|| "remote-note".into())
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .px_6()
        .text_size(px(theme::TEXT_BODY))
        .text_color(content(0.65))
        .child(text.into())
}

fn remote_name_field(renaming: &RemoteRenaming, cx: &mut Context<Pane>) -> gpui::Div {
    let mut fill = theme::pane_fill();
    fill.a = 1.;
    theme::field(if renaming.error.is_some() {
        danger()
    } else {
        accent()
    })
    .debug_selector(|| "remote-rename-field".into())
    .key_context("Rename")
    .on_action(
        cx.listener(|this, _: &CommitRename, window, cx| this.remote_commit_rename(window, cx)),
    )
    .on_action(cx.listener(|this, _: &CancelRename, window, cx| this.remote_end_rename(window, cx)))
    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    .w(px(260.))
    .flex_shrink_0()
    .px(px(6.))
    .bg(fill)
    .text_color(content(1.))
    .child(renaming.field.clone())
    .children(renaming.error.clone().map(|why| {
        div()
            .absolute()
            .left_0()
            .top(gpui::relative(1.))
            .mt(px(2.))
            .max_w(px(360.))
            .px_2()
            .rounded_full()
            .bg(fill)
            .border_1()
            .border_color(content(0.12))
            .text_size(px(theme::TEXT_SMALL))
            .text_color(danger())
            .truncate()
            .child(why)
    }))
}

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
    kept: Vec<Preview>,
}

impl Global for Downloads {}

impl super::Browser {
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
                            super::format_bytes(copied),
                            super::format_bytes(fetching.size)
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

    /// Disconnects server `id`, and every pane showing it says so.
    pub(super) fn disconnect_server(&mut self, id: &str, cx: &mut Context<Self>) {
        disconnect(id, cx);
        for pane in self
            .tabs
            .iter()
            .flat_map(|tab| tab.panes().cloned().collect::<Vec<_>>())
        {
            pane.update(cx, |pane, cx| pane.remote_disconnected(id, cx));
        }
        cx.notify();
    }

    /// Forgets server `id`: disconnects it and drops it from the saved
    /// list. Nothing on the server changes.
    pub(super) fn forget_server(&mut self, id: &str, cx: &mut Context<Self>) {
        disconnect(id, cx);
        self.sidebar.remotes.retain(|source| source.id != id);
        let id = id.to_owned();
        self.change_places(cx, None, move |store, _| store.forget_remote(&id).map(drop));
    }
}

/// A server folder as a drop names it: "srv on box".
fn on_server(at: &RemotePath) -> String {
    match at.name() {
        Some(name) => format!("{name} on {}", at.source()),
        None => at.source().to_owned(),
    }
}
