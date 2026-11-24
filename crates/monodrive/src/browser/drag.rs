//! The window's end of a drag of files: what starts each move, springs a
//! folder open under the pointer, and what a drop does.

use super::Browser;
use super::pane::Pane;
use super::pane::drag::*;
use super::pane::rows::text_width;
use crate::feedback::{self, Haptic};
use crate::motion;
use crate::preferences::Preferences;
use crate::system::Places;
use crate::theme::{self, Elevation, content};
use gpui::{
    Context, Div, DragMoveEvent, Entity, ExternalPaths, FontWeight, Window, div, prelude::*, px,
};
use monodrive_core::localsend::Outgoing;
use objc2_app_kit::NSEvent;
use std::path::PathBuf;

impl Browser {
    /// The window's end of a drag: what starts each move and what a drop does.
    pub(super) fn drags(&self, root: Div, cx: &mut Context<Self>) -> Div {
        root.on_drag_move::<FileDrag>(cx.listener(|this, _: &DragMoveEvent<FileDrag>, _, _| {
            this.drag.next_move();
        }))
        .on_drag_move::<ExternalPaths>(cx.listener(
            |this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                if !this.drag.next_move() {
                    let paths = event.drag(cx).paths().to_vec();
                    this.begin_external(paths, cx);
                }
            },
        ))
        // What's under the pointer offers itself again at release.
        .capture_any_mouse_up(cx.listener(|this, _, _, _| {
            this.drag.next_move();
        }))
        .on_drag_move::<ServerDrag>(cx.listener(|this, _: &DragMoveEvent<ServerDrag>, _, _| {
            this.drag.next_move();
        }))
        .on_drop::<FileDrag>(cx.listener(|this, _, window, cx| this.drop_files(window, cx)))
        .on_drop::<ServerDrag>(cx.listener(|this, _, window, cx| this.drop_files(window, cx)))
        .on_drop::<ExternalPaths>(cx.listener(|this, _, window, cx| this.drop_files(window, cx)))
        .on_modifiers_changed(cx.listener(|this, _, _, cx| {
            if this.drag.active() {
                cx.notify();
            }
        }))
    }

    /// Files from another app. Whether they're all folders takes the disk.
    fn begin_external(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        self.drag.begin_external(paths.clone());
        let drag = self.drag.clone();
        let checked = cx.background_executor().spawn(async move {
            let folders = paths.iter().all(|path| path.is_dir());
            (paths, folders)
        });
        cx.spawn(async move |_, _| {
            let (paths, folders) = checked.await;
            drag.folders_read(&paths, folders);
        })
        .detach();
    }

    /// Before a frame: the drag's state for what's held now, and what's still
    /// to be read about its volumes, off the UI thread. While the drag rests,
    /// the platform replays the last move, and AppKit its dragging updates, so
    /// the pill catches up with a scroll or a closed tab under the pointer.
    pub(super) fn settle_drag(&self, window: &Window, cx: &mut Context<Self>) {
        if !cx.has_active_drag() {
            if cx.has_platform_drag() {
                self.drag.away();
            } else if let Some(back) = self.drag.end() {
                cx.defer_in(window, |this, window, cx| {
                    this.spring_back(back, window, cx)
                });
            }
        }
        if let Some(tap) = self.drag.settle(held_keys(window)) {
            feedback::haptic(tap, cx);
        }
        self.read_volumes(cx);
        self.watch_button(window, cx);
        self.time_spring(window, cx);
    }

    /// Times the rest on what's under the pointer, with one timer, which a new
    /// place replaces. A drag out of the window is over nothing, which ends the timer.
    fn time_spring(&self, window: &Window, cx: &mut Context<Self>) {
        let wanted = cx
            .global::<Preferences>()
            .open_folders_while_dragging
            .then(|| self.drag.springable())
            .flatten();
        if !self.drag.spring_moved_to(&wanted) {
            return;
        }
        let Some(target) = wanted else {
            return;
        };
        self.start_spring(target, true, window, cx);
    }

    /// Space: what the drag rests on opens now, and the timer has nothing left.
    pub(super) fn spring_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !cx.global::<Preferences>().open_folders_while_dragging {
            return;
        }
        let Some(target) = self.drag.springable() else {
            return;
        };
        self.drag.spring_moved_to(&Some(target.clone()));
        self.start_spring(target, false, window, cx);
    }

    /// Opens `target` after the rest, if `waits`, and the pulse. Going in could
    /// raise a privacy prompt no one asked for, so a gated place stays shut,
    /// by where it really leads, and that's asked off the UI thread. A drop on
    /// it is still the person's own ask. The person may have moved on by the
    /// answer, so it opens only if the drag is still on `target`.
    fn start_spring(&self, target: Target, waits: bool, window: &Window, cx: &mut Context<Self>) {
        let drag = self.drag.clone();
        let drives = cx
            .try_global::<Places>()
            .map(|places| places.drives.clone());
        let timer = cx.spawn_in(window, async move |this, cx| {
            if waits {
                let rest = SPRING_DELAY - motion::PULSE;
                cx.background_executor().timer(rest).await;
            }
            let Target::Folder { path, .. } = &target else {
                return;
            };
            let path = path.clone();
            let gated = cx
                .background_executor()
                .spawn(async move { drives.is_some_and(|drives| drives.needs_consent(&path)) })
                .await;
            if gated {
                return;
            }
            if waits {
                drag.set_pulsing(true);
                let _ = this.update(cx, |_, cx| cx.notify());
                cx.background_executor().timer(motion::PULSE).await;
                drag.set_pulsing(false);
            }
            let _ = this.update_in(cx, |this, window, cx| {
                if this.drag.springable().as_ref() == Some(&target) {
                    this.spring(&target, window, cx);
                }
            });
        });
        self.drag.keep_spring_timer(timer);
    }

    /// Opens `target` under the drag, which goes on over the new listing. What
    /// the pill showed there no longer counts once the listing lands, since a
    /// release checks what's under the pointer.
    fn spring(&mut self, target: &Target, window: &mut Window, cx: &mut Context<Self>) {
        let Target::Folder { spot, path, .. } = target else {
            return;
        };
        if !cx.has_active_drag() {
            return;
        }
        self.drag
            .remember_tab(self.tabs[self.active].pane.entity_id());
        let opens_in = match spot {
            Spot::Tab(ix) => {
                if *ix != self.active && *ix < self.tabs.len() {
                    self.show_tab(*ix, window, cx);
                    feedback::haptic(Haptic::LevelChange, cx);
                }
                return;
            }
            Spot::Row(id, _) => self
                .shown_panes()
                .into_iter()
                .find(|pane| pane.entity_id() == *id),
            Spot::Place(_) => Some(self.pane().clone()),
            Spot::Pane(_) | Spot::Crumb(..) | Spot::Device(_) => None,
        };
        let Some(pane) = opens_in.filter(|pane| pane.read(cx).folder() != path) else {
            return;
        };
        self.drag
            .remember_pane(pane.entity_id(), || pane.read(cx).before_spring());
        pane.update(cx, |pane, cx| pane.navigate(path.clone(), cx));
        feedback::haptic(Haptic::LevelChange, cx);
    }

    /// Esc: the tab and the panes as they were, Back history and all.
    pub(super) fn spring_back(
        &mut self,
        back: SpringBack,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(tab) = back.tab
            && let Some(ix) = self
                .tabs
                .iter()
                .position(|t| t.panes().any(|p| p.entity_id() == tab))
            && ix != self.active
        {
            // Showing the tab reloads a search, which would pick its first hit.
            let panes: Vec<_> = self.tabs[ix].panes().cloned().collect();
            for pane in panes {
                pane.update(cx, |pane, _| pane.keep_hits_selected());
            }
            self.show_tab(ix, window, cx);
        }
        let open: Vec<Entity<Pane>> = self
            .tabs
            .iter()
            .flat_map(|tab| tab.panes().cloned().collect::<Vec<_>>())
            .collect();
        for (id, to) in back.panes {
            if let Some(pane) = open.iter().find(|pane| pane.entity_id() == id) {
                pane.update(cx, |pane, cx| pane.spring_back(to, cx));
            }
        }
        cx.notify();
    }

    /// A mouse-up can go missing, as GPUI's own drag replay allows for when a
    /// native menu takes one, and a drag then has nothing to end it. So while
    /// the window owns a drag, the button itself is what says it's over. AppKit
    /// ends its own drags. A window with no AppKit behind it, as in tests, has
    /// only its events.
    fn watch_button(&self, window: &Window, cx: &mut Context<Self>) {
        if crate::traffic_lights::ns_window(window).is_none()
            || cx.has_platform_drag()
            || !self.drag.needs_button_watch()
        {
            return;
        }
        let watch = cx.spawn_in(window, async move |_, cx| {
            loop {
                cx.background_executor().timer(BUTTON_CHECK).await;
                let held = NSEvent::pressedMouseButtons() & 1 != 0;
                if !held {
                    let _ = cx.update(release_lost_drag);
                    return;
                }
            }
        });
        self.drag.keep_button_watch(watch);
    }

    /// Starts what this drag doesn't know yet about its volumes. Each answer
    /// draws a frame, and until then `decide` hears "not the same".
    fn read_volumes(&self, cx: &mut Context<Self>) {
        let drives = || {
            cx.try_global::<Places>()
                .map(|places| places.drives.clone())
        };
        let sources = self
            .drag
            .sources_to_read()
            .map(|sources| (drives(), sources));
        let target = self.drag.target_to_read().map(|folder| (drives(), folder));
        if let Some((drives, sources)) = sources {
            let drag = self.drag.clone();
            let read = cx.background_executor().spawn(async move {
                let devices = devices_of(drives.as_ref(), &sources);
                (sources, devices)
            });
            cx.spawn(async move |this, cx| {
                let (sources, devices) = read.await;
                drag.sources_read(&sources, devices);
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
        if let Some((drives, folder)) = target {
            let drag = self.drag.clone();
            let read = cx.background_executor().spawn(async move {
                let device = device_of(drives.as_ref(), &folder);
                (folder, device)
            });
            cx.spawn(async move |this, cx| {
                let (folder, device) = read.await;
                drag.target_read(&folder, device);
                let _ = this.update(cx, |_, cx| cx.notify());
            })
            .detach();
        }
    }

    /// The button came up over the window. Nothing has touched the disk until
    /// now; the job checks each item again as it starts.
    fn drop_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.drag.finish() {
            Some(Decision::Send {
                paths,
                into,
                moving,
                ..
            }) => {
                feedback::haptic(Haptic::Generic, cx);
                self.pane()
                    .update(cx, |pane, cx| pane.send(paths, into, moving, window, cx));
            }
            Some(Decision::Pin { paths, at }) => {
                feedback::haptic(Haptic::Generic, cx);
                self.pin_folders(paths, at, cx);
            }
            Some(Decision::Upload { paths, into, .. }) => {
                feedback::haptic(Haptic::Generic, cx);
                self.pane()
                    .update(cx, |pane, cx| pane.upload(paths, into, window, cx));
            }
            Some(Decision::Download { paths, into, .. }) => {
                feedback::haptic(Haptic::Generic, cx);
                self.pane().update(cx, |pane, cx| {
                    pane.download_from_server(paths, into, window, cx)
                });
            }
            Some(Decision::AirDrop(paths)) => {
                feedback::haptic(Haptic::Generic, cx);
                self.pane().update(cx, |pane, cx| pane.airdrop(&paths, cx));
            }
            Some(Decision::LocalSend { paths, device }) => {
                feedback::haptic(Haptic::Generic, cx);
                crate::localsend::get(cx).update(cx, |localsend, cx| {
                    localsend.send(*device, Outgoing::Files(paths), None, cx)
                });
            }
            Some(Decision::ChooseDevice(paths)) => {
                feedback::haptic(Haptic::Generic, cx);
                self.pane()
                    .update(cx, |pane, cx| pane.send_with_localsend(paths, window, cx));
            }
            Some(Decision::Nothing(_)) | None => {}
        }
    }

    /// The pill by the pointer: what a drop here would do.
    pub(super) fn drag_pill(&self, window: &Window) -> Option<impl IntoElement + use<>> {
        let (text, acts) = self
            .drag
            .decision(|_, decision| (decision.says(), !matches!(decision, Decision::Nothing(_))))?;
        let at = window.mouse_position();
        let room = window.viewport_size();
        // Its text, its side padding and its border.
        let width = (text_width(&text, theme::TEXT_BODY, FontWeight::NORMAL, window) + 2. * 13.)
            .min(PILL_WIDEST);
        // Past the window's edge, it flips to the pointer's other side.
        let right = at.x.as_f32() + PILL_OFFSET.x;
        let x = if right + width > room.width.as_f32() {
            at.x.as_f32() - PILL_OFFSET.x - width
        } else {
            right
        };
        let below = at.y.as_f32() + PILL_OFFSET.y;
        let y = if below + PILL_HEIGHT > room.height.as_f32() {
            at.y.as_f32() - PILL_OFFSET.y - PILL_HEIGHT
        } else {
            below
        };
        Some(
            theme::floating(Elevation::Popover)
                .absolute()
                .left(px(x))
                .top(px(y))
                .max_w(px(PILL_WIDEST))
                .h(px(PILL_HEIGHT))
                .px_3()
                .flex()
                .items_center()
                .child(
                    div()
                        .relative()
                        .min_w_0()
                        .truncate()
                        .text_size(px(theme::TEXT_BODY))
                        .text_color(content(if acts { 1. } else { 0.65 }))
                        .child(text),
                ),
        )
    }
}
