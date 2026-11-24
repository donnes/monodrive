//! The sheets over the window, one at a time: Settings, the palette, Go to
//! Folder and the questions that ask before something happens.

use super::Browser;
use super::pane::searches::SearchMenu;
use super::with_owner;
use crate::commands::{self};
use crate::connect::ConnectView;
use crate::empty_trash::{self, EmptyTrashView};
use crate::goto::{GotoEvent, GotoView};
use crate::localsend;
use crate::localsend_ask::{OfferView, PinView};
use crate::palette::{Now, PaletteEvent, PaletteView};
use crate::preferences::Preferences;
use crate::remove_worktree::{self, RemoveWorktreeView};
use crate::review_duplicates::{self, ReviewDuplicatesView};
use crate::settings::{self, SettingsView};
use crate::system::Places;
use gpui::{App, Context, Entity, Focusable, Window, prelude::*};
use monodrive_core::Origin;
use monodrive_core::ops::{self, Request};
use std::path::PathBuf;

/// The sheet over the window, one at a time.
pub(super) enum Sheet {
    Settings(Entity<SettingsView>),
    Palette(Entity<PaletteView>),
    Goto(Entity<GotoView>),
    /// The Activity popover; `jobs` draws it.
    Jobs,
    /// The updates popover; `updates` draws it.
    Updates,
    /// The search field's menu.
    Searches(Entity<SearchMenu>),
    EmptyTrash(Entity<EmptyTrashView>),
    LocalSendOffer(Entity<crate::localsend_ask::OfferView>),
    LocalSendPin(Entity<crate::localsend_ask::PinView>),
    RemoveWorktree(Entity<RemoveWorktreeView>),
    ReviewDuplicates(Entity<ReviewDuplicatesView>),
    Connect(Entity<ConnectView>),
}

/// What a command chosen in the palette would act on.
#[derive(PartialEq)]
pub(super) struct PaletteTarget {
    folder: PathBuf,
    /// What's selected, whatever order the rows are in.
    chosen: Vec<PathBuf>,
    /// The sidebar's row, while the sidebar has the keys.
    sidebar: Option<super::sidebar::RowTarget>,
}

/// What needs the browser's commands back. A frame with a sheet holds them
/// off, and running a command or checking what would run reads the drawn frame.
pub(super) enum AfterSheet {
    Run(Box<dyn gpui::Action>),
    OpenPalette,
}

impl Browser {
    /// Opens or closes a sheet; the jobs popover's list is kept fresh while
    /// it's open.
    pub(super) fn set_sheet(&mut self, sheet: Option<Sheet>, cx: &mut Context<Self>) {
        let jobs = matches!(sheet, Some(Sheet::Jobs));
        // A sheet closes a peeked card, which then never hears the pointer
        // leave.
        if sheet.is_some() {
            self.updates_peek.close();
            self.jobs_peek.close();
        }
        self.sheet = sheet;
        self.jobs.update(cx, |list, cx| list.set_open(jobs, cx));
        cx.notify();
    }

    pub(super) fn close_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_sheet(None, cx);
        let back = self
            .focus_before_sheet
            .take()
            .unwrap_or_else(|| self.pane().read(cx).list_focus.clone());
        window.focus(&back, cx);
        cx.notify();
    }

    pub(super) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Settings(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        self.remember_focus(window, cx);
        let view = cx.new(SettingsView::new);
        cx.subscribe_in(
            &view,
            window,
            |this, _, _: &settings::Dismissed, window, cx| this.close_sheet(window, cx),
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::Settings(view)), cx);
    }

    pub(super) fn open_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Goto(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        self.remember_focus(window, cx);
        // Not from a location that's unavailable: its path may be another's now.
        let folder = self.pane().read(cx).goto_from();
        let view = cx.new(|cx| GotoView::new(folder, cx));
        cx.subscribe_in(&view, window, |this, _, event: &GotoEvent, window, cx| {
            let destination = match event {
                GotoEvent::Go(destination) => Some((
                    destination.folder.clone(),
                    destination.select.iter().cloned().collect(),
                )),
                GotoEvent::Dismissed => None,
            };
            this.close_sheet(window, cx);
            if let Some((folder, select)) = destination {
                this.pane()
                    .update(cx, |pane, cx| pane.go_to(folder, select, window, cx));
            }
        })
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::Goto(view)), cx);
    }

    /// Opens the palette on what would run now, which is read off the frame
    /// drawn last. Another sheet closes first, and the palette waits for it.
    pub(super) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Palette(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        if self.sheet_open() {
            self.close_sheet(window, cx);
            self.after_sheet = Some(AfterSheet::OpenPalette);
            return;
        }
        self.remember_focus(window, cx);
        let origin = self
            .focus_before_sheet
            .clone()
            .unwrap_or_else(|| self.pane().read(cx).list_focus.clone());
        // Quit and the like listen on the app, where only this list finds them.
        let anywhere = window.available_actions(cx);
        let available = commands::all()
            .iter()
            .map(|command| (command.action)())
            .filter(|action| {
                window.is_action_available_in(&**action, &origin)
                    || anywhere.iter().any(|a| a.name() == action.name())
            })
            .map(|action| action.name())
            .collect();
        let now = Now {
            available,
            scopes: self.pane().update(cx, |pane, _| pane.scopes()),
            favorites: self.sidebar.places.clone(),
            volumes: self.sidebar.all_volumes.clone(),
            servers: self.sidebar.servers.clone(),
            files: self.pane().read(cx).chosen(),
            index: self.index.clone(),
        };
        self.palette_target = Some(self.target(window, cx));
        let view = cx.new(|cx| PaletteView::new(now, cx));
        cx.subscribe_in(&view, window, Self::palette_event).detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::Palette(view)), cx);
    }

    /// What commands would act on now.
    pub(super) fn target(&self, window: &Window, cx: &App) -> PaletteTarget {
        let pane = self.pane().read(cx);
        let mut chosen = pane.chosen();
        chosen.sort();
        PaletteTarget {
            folder: pane.folder().to_path_buf(),
            chosen,
            sidebar: self
                .sidebar_focus
                .is_focused(window)
                .then(|| self.sidebar_target())
                .flatten(),
        }
    }

    /// Runs a command chosen in the palette, unless it would act on something
    /// other than what was there when the palette opened: a job that finishes
    /// meanwhile selects what it made, and that isn't what the person chose for.
    pub(super) fn run_from_palette(
        &mut self,
        action: Box<dyn gpui::Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let opened_on = self.palette_target.take();
        if commands::acts_on_target(action.name())
            && opened_on.is_some_and(|target| target != self.target(window, cx))
        {
            let said = "The selection changed while the palette was open.";
            return self.toast(said.into(), true, None, cx);
        }
        window.dispatch_action(action, cx);
    }

    /// What the palette asked for, once it has closed and focus is back.
    pub(super) fn palette_event(
        &mut self,
        _: &Entity<PaletteView>,
        event: &PaletteEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_sheet(window, cx);
        match event {
            PaletteEvent::Dismissed => {}
            PaletteEvent::Run(action) => {
                self.after_sheet = Some(AfterSheet::Run(action.boxed_clone()));
            }
            PaletteEvent::Go(folder) => self.pane().update(cx, |pane, cx| {
                pane.go_to(folder.clone(), Vec::new(), window, cx)
            }),
            PaletteEvent::Open(file) => self.pane().update(cx, |pane, cx| pane.open_file(file, cx)),
            PaletteEvent::Eject(mount) => self.eject(mount.clone(), cx),
            PaletteEvent::Connect(address) => self.connect_to(address, window, cx),
            PaletteEvent::OpenServer(id) => self
                .pane()
                .update(cx, |pane, cx| pane.open_server(id, window, cx)),
            PaletteEvent::Disconnect(id) => self.disconnect_server(id, cx),
            PaletteEvent::OpenWith { files, app } => {
                self.open_files_with(files.clone(), app.clone(), cx)
            }
            PaletteEvent::OtherApp(files) => self.open_files_with_other(files.clone(), cx),
        }
    }

    /// Empty Trash: erases what's in the Trash for good, once a person has
    /// confirmed how many items that is.
    pub(super) fn empty_trash(&mut self, window: &Window, cx: &mut Context<Self>) {
        with_owner(
            window,
            cx,
            |owner| owner.trash_items(),
            |this, items, window, cx| match items {
                Ok(items) if items.is_empty() => {
                    this.toast("The Trash is empty.".into(), false, None, cx)
                }
                Ok(items) => this.ask_empty_trash(items, window, cx),
                Err(e) => this.toast(e, true, None, cx),
            },
        );
    }

    pub(super) fn ask_empty_trash(
        &mut self,
        items: Vec<ops::TrashItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remember_focus(window, cx);
        let view = cx.new(|cx| EmptyTrashView::new(items, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, answer: &empty_trash::Answer, window, cx| {
                let items = match answer {
                    empty_trash::Answer::Empty(items) => Some(items.clone()),
                    empty_trash::Answer::Cancel => None,
                };
                this.close_sheet(window, cx);
                if let Some(items) = items {
                    with_owner(
                        window,
                        cx,
                        move |owner| owner.submit(Origin::Window, Request::EmptyTrash { items }),
                        |this, queued, _, cx| match queued {
                            Ok(queued) => {
                                this.jobs.update(cx, |jobs, cx| jobs.follow(queued.op, cx))
                            }
                            Err(e) => this.toast(e, true, None, cx),
                        },
                    );
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::EmptyTrash(view)), cx);
    }

    /// Asks before a worktree goes, with what the active pane worked out:
    /// what goes, what's at risk, or why it stays.
    pub(super) fn ask_remove_worktree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ask) = self
            .pane()
            .update(cx, |pane, _| pane.take_worktree_question())
        else {
            return;
        };
        self.remember_focus(window, cx);
        let view = cx.new(|cx| RemoveWorktreeView::new(ask, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, answer: &remove_worktree::Answer, window, cx| {
                let request = match answer {
                    remove_worktree::Answer::Remove(request) => Some(request.clone()),
                    remove_worktree::Answer::Cancel => None,
                };
                this.close_sheet(window, cx);
                if let Some(request) = request {
                    this.pane()
                        .update(cx, |pane, cx| pane.submit(request, window, cx));
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::RemoveWorktree(view)), cx);
    }

    /// Lists the copies Find Duplicates would move, from the active pane,
    /// before any of them do.
    pub(super) fn review_duplicates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = self
            .pane()
            .update(cx, |pane, _| pane.take_duplicates_review())
        else {
            return;
        };
        self.remember_focus(window, cx);
        let view = cx.new(|cx| ReviewDuplicatesView::new(review, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, answer: &review_duplicates::Answer, window, cx| {
                let request = match answer {
                    review_duplicates::Answer::Trash(request) => Some(request.clone()),
                    review_duplicates::Answer::Cancel => None,
                };
                this.close_sheet(window, cx);
                if let Some(request) = request {
                    this.pane()
                        .update(cx, |pane, cx| pane.trash_duplicates(request, window, cx));
                }
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::ReviewDuplicates(view)), cx);
    }

    /// Asks what LocalSend needs a person for: an offer, in every window
    /// until one answers it, and a PIN, in the window in front.
    pub(super) fn ask_localsend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Waits for the sheet on screen, so a question never takes the keys
        // from another.
        if self.sheet.is_some() {
            return;
        }
        let entity = localsend::get(cx);
        if let Some(offer) = entity.read(cx).offers.first().cloned() {
            let folder = cx.global::<Preferences>().localsend_save_to();
            self.remember_focus(window, cx);
            let view = cx.new(|cx| OfferView::new(offer, folder, cx));
            cx.subscribe_in(
                &view,
                window,
                |this, _, _: &crate::localsend_ask::Done, window, cx| this.close_sheet(window, cx),
            )
            .detach();
            window.focus(&view.focus_handle(cx), cx);
            return self.set_sheet(Some(Sheet::LocalSendOffer(view)), cx);
        }
        if !window.is_window_active() {
            return;
        }
        let Some(wanted) = entity.update(cx, |localsend, _| localsend.pin_wanted.take()) else {
            return;
        };
        self.remember_focus(window, cx);
        let view = cx.new(|cx| PinView::new(wanted.device, wanted.outgoing, wanted.wrong, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, _: &crate::localsend_ask::Done, window, cx| this.close_sheet(window, cx),
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::LocalSendPin(view)), cx);
    }

    /// The Connect to Server card on `address`, connecting at once, as a
    /// saved server chosen in the palette does.
    pub(super) fn connect_to(
        &mut self,
        address: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_connect(window, cx);
        if let Some(Sheet::Connect(view)) = &self.sheet {
            view.update(cx, |view, cx| view.connect_to(address, cx));
        }
    }

    /// ⇧⌘K: the Connect to Server card, with the saved servers as last read.
    pub(super) fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Sheet::Connect(view)) = &self.sheet {
            window.focus(&view.focus_handle(cx), cx);
            return;
        }
        let Some(places) = cx.try_global::<Places>().cloned() else {
            return;
        };
        self.remember_focus(window, cx);
        let saved = self.sidebar.servers.clone();
        let remotes = self.sidebar.remotes.clone();
        let view = cx.new(|cx| crate::connect::ConnectView::new(places, saved, &remotes, cx));
        cx.subscribe_in(
            &view,
            window,
            |this, _, event: &crate::connect::ConnectEvent, window, cx| {
                this.close_sheet(window, cx);
                match event {
                    crate::connect::ConnectEvent::Connected { address, mount } => {
                        this.toast(
                            format!("Connected {address} at {}", mount.display()),
                            false,
                            None,
                            cx,
                        );
                        this.load_volumes(cx);
                    }
                    crate::connect::ConnectEvent::ConnectedServer(id) => this
                        .pane()
                        .update(cx, |pane, cx| pane.open_server(id, window, cx)),
                    crate::connect::ConnectEvent::Dismissed => {}
                }
                this.load_lists(cx);
            },
        )
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::Connect(view)), cx);
    }
}
