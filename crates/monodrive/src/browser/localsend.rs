//! The LocalSend page: who this Mac is to others, the devices nearby to drop
//! files on or click to send to, and whether this Mac receives. Laid out as
//! Finder's AirDrop page is.

use super::drag::{Spot, Target, offers};
use super::pane::{Pane, format_date};
use super::{Browser, FocusNext, FocusPrevious, Offer, Sheet, format_bytes};
use crate::localsend::{self, Direction, LocalSend, Receiving, Said, count};
use crate::localsend_ask::{Done, OfferView, PinView};
use crate::menu;
use crate::pasteboard::{self, Pasteboard};
use crate::preferences::{self, Preferences};
use crate::session::Shows;
use crate::theme::{self, ButtonLook, content, danger, text_button};
use gpui::{
    AnyElement, App, AppContext, ClipboardEntry, Context, Entity, FocusHandle, Focusable,
    FontWeight, Hsla, Image, PathPromptOptions, Role, Subscription, Toggled, Window, deferred, div,
    prelude::*, px,
};
use monodrive_core::localsend::{Device, Kind, Outgoing};
use std::path::PathBuf;

/// What the clipboard holds, as a device's menu offers it, or what the page
/// holds for the device clicked next.
pub(super) enum Clipped {
    Ready(Outgoing),
    /// Made ready to send once it's picked.
    Picture(Image),
}

/// The menu a click on a device opens: what to send it.
pub(super) struct DeviceMenu {
    /// The tile it opened under.
    tile: usize,
    device: Device,
    /// The row under the keys or the pointer.
    hot: Option<usize>,
    clipped: Option<Clipped>,
    focus: FocusHandle,
}

const CHOOSE_FILES: usize = 0;
const SEND_CLIPBOARD: usize = 1;

/// Looking for devices while the page is on screen.
pub(super) struct LocalSendPage {
    /// Repaints as devices come and go, while looking.
    looking: Option<Subscription>,
    /// What Send with LocalSend chose or Paste held, for the device clicked
    /// next.
    pub(super) sending: Option<Clipped>,
    pub(super) menu: Option<DeviceMenu>,
    /// The pointer is on the tile whose menu is open, whose press closes it
    /// rather than counting as one outside.
    on_menu_tile: bool,
    /// Waits for LocalSend to be picked in the share menu on screen.
    pub(super) sharing: gpui::Task<()>,
    /// The received text's popover is open.
    pub(super) history: bool,
    /// The pointer is on the button that opens it, whose press toggles it
    /// rather than counting as one outside.
    on_history_button: bool,
}

impl Default for LocalSendPage {
    fn default() -> Self {
        Self {
            looking: None,
            sending: None,
            menu: None,
            on_menu_tile: false,
            sharing: gpui::Task::ready(()),
            history: false,
            on_history_button: false,
        }
    }
}

impl LocalSendPage {
    pub(super) fn looking(&self) -> bool {
        self.looking.is_some()
    }
}

/// For a pane going away, which can't be updated any more.
pub(super) fn stop_looking(cx: &mut gpui::App) {
    localsend::get(cx).update(cx, |localsend, _| localsend.stop_looking());
}

impl Pane {
    pub(super) fn showing_localsend(&self) -> bool {
        self.query.is_empty() && self.stop().shows == Shows::LocalSend
    }

    /// Starts looking for devices, once.
    pub(super) fn enter_localsend(&mut self, cx: &mut Context<Self>) {
        if self.localsend.looking.is_some() {
            return;
        }
        let localsend = localsend::get(cx);
        self.localsend.looking = Some(cx.observe(&localsend, |_, _, cx| cx.notify()));
        localsend.update(cx, |localsend, cx| localsend.look(cx));
    }

    /// Stops looking, and lets go of what the page holds.
    pub(super) fn leave_localsend(&mut self, cx: &mut Context<Self>) {
        self.localsend.sending = None;
        self.localsend.menu = None;
        self.localsend.history = false;
        if self.localsend.looking.take().is_some() {
            localsend::get(cx).update(cx, |localsend, _| localsend.stop_looking());
        }
    }

    /// Opens the page holding `paths`, for the device clicked next.
    pub(super) fn send_with_localsend(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_page(Shows::LocalSend, window, cx);
        self.localsend.sending =
            (!paths.is_empty()).then_some(Clipped::Ready(Outgoing::Files(paths)));
        cx.notify();
    }

    /// Paste on the page: Send Clipboard while a device's menu has the keys,
    /// or else holds what the clipboard holds for the device clicked next.
    pub(super) fn paste_into_localsend(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let menu = self.localsend.menu.as_ref();
        if menu.is_some_and(|menu| menu.focus.is_focused(window)) {
            return self.run_menu(SEND_CLIPBOARD, window, cx);
        }
        self.localsend.menu = None;
        match clipboard(cx) {
            Some(clipped) => {
                self.localsend.sending = Some(clipped);
                cx.notify();
            }
            None => self.toast("The clipboard is empty.".into(), false, None, cx),
        }
    }

    /// Sends `clipped` to `device`: a picture once it's turned into a file
    /// phones take, off the UI thread.
    fn send_clipped(&mut self, device: Device, clipped: Clipped, cx: &mut Context<Self>) {
        let image = match clipped {
            Clipped::Ready(outgoing) => return send(device, outgoing, cx),
            Clipped::Picture(image) => image,
        };
        let name = chrono::Local::now().format("Clipboard %Y-%m-%d at %H.%M.%S");
        let converted = cx.background_spawn(async move { pasteboard::shareable(image) });
        cx.spawn(async move |this, cx| {
            let (extension, bytes) = converted.await;
            let name = format!("{name}.{extension}");
            let _ = this.update(cx, |_, cx| {
                send(device, Outgoing::Bytes { name, bytes }, cx)
            });
        })
        .detach();
    }

    /// A click on a device: sends what the page holds, or opens the device's
    /// menu, or closes it.
    fn click_device(
        &mut self,
        tile: usize,
        device: Device,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(held) = self.localsend.sending.take() {
            return self.send_clipped(device, held, cx);
        }
        if self
            .close_menu(window, cx)
            .is_some_and(|menu| menu.tile == tile)
        {
            return;
        }
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.localsend.menu = Some(DeviceMenu {
            tile,
            device,
            hot: None,
            clipped: clipboard(cx),
            focus,
        });
        self.localsend.on_menu_tile = true;
        cx.notify();
    }

    /// Closes the device's menu, giving the keys back to the page.
    fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<DeviceMenu> {
        let menu = self.localsend.menu.take()?;
        window.focus(&self.list_focus, cx);
        cx.notify();
        Some(menu)
    }

    fn step_menu(&mut self, down: bool, cx: &mut Context<Self>) {
        let Some(menu) = self.localsend.menu.as_mut() else {
            return;
        };
        let last = if menu.clipped.is_some() {
            SEND_CLIPBOARD
        } else {
            CHOOSE_FILES
        };
        menu.hot = Some(match (menu.hot, down) {
            (None, true) => CHOOSE_FILES,
            (None, false) => last,
            (Some(hot), true) if hot < last => hot + 1,
            (Some(_), true) => CHOOSE_FILES,
            (Some(hot), false) if hot > CHOOSE_FILES => hot - 1,
            (Some(_), false) => last,
        });
        cx.notify();
    }

    fn run_menu(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let empty = |menu: &DeviceMenu| row == SEND_CLIPBOARD && menu.clipped.is_none();
        if self.localsend.menu.as_ref().is_none_or(empty) {
            return;
        }
        let Some(menu) = self.close_menu(window, cx) else {
            return;
        };
        match menu.clipped {
            Some(clipped) if row == SEND_CLIPBOARD => self.send_clipped(menu.device, clipped, cx),
            _ => self.choose_files(menu.device, window, cx),
        }
    }

    /// Asks for files and folders, then sends them to `device`.
    fn choose_files(&mut self, device: Device, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Send".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |_, cx| send(device, Outgoing::Files(paths), cx));
        })
        .detach();
    }

    /// A device's menu, under its tile: Choose Files…, then Send Clipboard
    /// with what it holds.
    fn device_menu(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let menu = self.localsend.menu.as_ref()?;
        let hot = menu.hot;
        let files = theme::row_slot("localsend-menu-files")
            .debug_selector(|| "localsend-menu-files".into())
            .role(Role::MenuItem)
            .aria_label("Choose Files…")
            .aria_selected(hot == Some(CHOOSE_FILES))
            .on_mouse_move(cx.listener(|this, _, _, cx| this.hover_menu(CHOOSE_FILES, cx)))
            .on_click(cx.listener(|this, _, window, cx| this.run_menu(CHOOSE_FILES, window, cx)))
            .child(
                theme::row(hot == Some(CHOOSE_FILES))
                    .child(
                        theme::icon("folder-01", 16.)
                            .text_color(theme::row_muted(hot == Some(CHOOSE_FILES))),
                    )
                    .child(div().flex_1().min_w_0().truncate().child("Choose Files…")),
            );
        let enabled = menu.clipped.is_some();
        let active = hot == Some(SEND_CLIPBOARD);
        let ink = if enabled {
            theme::row_muted(active)
        } else {
            content(0.3)
        };
        let clipboard = theme::row_slot("localsend-menu-clipboard")
            .debug_selector(|| "localsend-menu-clipboard".into())
            .role(Role::MenuItem)
            .aria_label("Send Clipboard")
            .aria_selected(active)
            .aria_keyshortcuts("⌘V")
            .when(!enabled, |d| d.cursor_default())
            .when(enabled, |d| {
                d.on_mouse_move(cx.listener(|this, _, _, cx| this.hover_menu(SEND_CLIPBOARD, cx)))
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.run_menu(SEND_CLIPBOARD, window, cx)
                        }),
                    )
            })
            .child(
                theme::row(active)
                    .h(px(theme::TALL_ROW_HEIGHT))
                    .when(!enabled, |d| d.text_color(content(0.3)))
                    .child(theme::icon("clipboard", 16.).text_color(ink))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child("Send Clipboard"))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(theme::TEXT_CAPTION))
                                    .text_color(ink)
                                    .child(clipped_line(menu.clipped.as_ref())),
                            ),
                    )
                    .child(crate::menu::shortcut("⌘V".into(), ink)),
            );
        Some(
            theme::inset_rows(
                theme::floating(theme::Elevation::Popover)
                    .id("localsend-device-menu")
                    .debug_selector(|| "localsend-device-menu".into())
                    .key_context("ContextMenu")
                    .track_focus(&menu.focus)
                    .role(Role::Menu)
                    .aria_label(format!("Send to {}", menu.device.alias))
                    .w(px(280.))
                    .occlude()
                    .on_action(cx.listener(|this, _: &menu::Up, _, cx| this.step_menu(false, cx)))
                    .on_action(cx.listener(|this, _: &menu::Down, _, cx| this.step_menu(true, cx)))
                    .on_action(cx.listener(|this, _: &menu::Run, window, cx| {
                        if let Some(hot) = this.localsend.menu.as_ref().and_then(|m| m.hot) {
                            this.run_menu(hot, window, cx);
                        }
                    }))
                    .on_action(cx.listener(|this, _: &menu::Dismiss, window, cx| {
                        this.close_menu(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &FocusNext, window, cx| {
                        this.close_menu(window, cx);
                    }))
                    .on_action(cx.listener(|this, _: &FocusPrevious, window, cx| {
                        this.close_menu(window, cx);
                    }))
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                        if !this.localsend.on_menu_tile {
                            this.close_menu(window, cx);
                        }
                    }))
                    .flex()
                    .flex_col(),
            )
            .child(files)
            .child(clipboard),
        )
    }

    fn hover_menu(&mut self, row: usize, cx: &mut Context<Self>) {
        if let Some(menu) = self.localsend.menu.as_mut().filter(|m| m.hot != Some(row)) {
            menu.hot = Some(row);
            cx.notify();
        }
    }

    pub(super) fn localsend_page(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.showing_localsend() {
            return None;
        }
        if !cx.global::<Preferences>().localsend {
            return Some(turned_off().into_any_element());
        }
        let entity = localsend::get(cx);
        let shown = entity.read(cx);
        let body = if shown.devices.is_empty() {
            empty(shown.scanning).into_any_element()
        } else {
            div()
                .id("localsend-devices")
                .flex_1()
                .overflow_y_scroll()
                .p(px(theme::PANE_INSET))
                .flex()
                .flex_wrap()
                .content_start()
                .gap_2()
                .children(
                    shown
                        .devices
                        .clone()
                        .into_iter()
                        .enumerate()
                        .map(|(ix, device)| self.device(ix, device, cx)),
                )
                .into_any_element()
        };
        Some(
            div()
                .id("localsend-page")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(self.localsend_header(&entity, cx))
                .child(body)
                .child(footer(&entity, cx))
                .into_any_element(),
        )
    }

    fn localsend_header(
        &self,
        entity: &Entity<LocalSend>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let shown = entity.read(cx);
        let me = shown.alias().unwrap_or("This Mac").to_owned();
        let scanning = shown.scanning;
        let held = &self.localsend.sending;
        let name = match held {
            None => me.clone(),
            Some(Clipped::Picture(_)) => "Picture".into(),
            Some(Clipped::Ready(Outgoing::Files(paths))) => match paths.as_slice() {
                [one] => one
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                items => count(items.len(), "item"),
            },
            Some(Clipped::Ready(Outgoing::Text(text))) => one_line(text),
            Some(Clipped::Ready(Outgoing::Bytes { name, .. })) => name.clone(),
        };
        let holding = held.is_some();
        let line = if holding {
            "Click a device to send it"
        } else {
            "Others see this Mac as"
        };
        div()
            .flex()
            .items_center()
            .gap_3()
            .px(px(theme::PANE_INSET))
            .py_4()
            .border_b_1()
            .border_color(content(0.06))
            .child(avatar(&me))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(theme::TEXT_SMALL))
                            .text_color(content(0.65))
                            .child(line),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(px(theme::TEXT_TITLE))
                            .font_weight(FontWeight::MEDIUM)
                            .child(name),
                    ),
            )
            .when(holding, |d| {
                d.child(
                    text_button("localsend-let-go", "Cancel", ButtonLook::Secondary).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.localsend.sending = None;
                            cx.notify();
                        }),
                    ),
                )
            })
            .child(
                text_button(
                    "localsend-scan",
                    "Scan Again",
                    if scanning {
                        ButtonLook::Disabled
                    } else {
                        ButtonLook::Secondary
                    },
                )
                .when(!scanning, |d| {
                    d.on_click(cx.listener(|_, _, _, cx| {
                        localsend::get(cx).update(cx, |localsend, cx| localsend.scan(cx))
                    }))
                }),
            )
            .child(self.history_button(entity, cx))
    }

    /// The clock that opens the text devices sent, under it.
    fn history_button(
        &self,
        entity: &Entity<LocalSend>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let open = self.localsend.history;
        div()
            .relative()
            .flex_shrink_0()
            .child(theme::tip_below(
                theme::icon_button("localsend-history", "Received Text", "clock", true)
                    .debug_selector(|| "localsend-history".into())
                    .when(open, |d| d.bg(content(0.14)))
                    .on_hover(cx.listener(|this, over: &bool, _, _| {
                        this.localsend.on_history_button = *over;
                    }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.localsend.history = !this.localsend.history;
                        cx.notify();
                    })),
                "Received Text",
            ))
            .when(open, |d| {
                d.child(deferred(history(entity, cx)).with_priority(1))
            })
    }

    fn device(
        &self,
        ix: usize,
        device: Device,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let spot = Spot::Device(ix);
        let outlined = self.drag.outlines(&spot);
        let name = device.alias.clone();
        let drag = self.drag.clone();
        let target = Target::LocalSend {
            spot,
            device: Box::new(device.clone()),
        };
        let tint = tint_of(&device.fingerprint);
        let detail = device
            .model
            .clone()
            .unwrap_or_else(|| kind_name(device.kind).into());
        let picture = kind_icon(device.kind);
        let open = self
            .localsend
            .menu
            .as_ref()
            .is_some_and(|menu| menu.tile == ix);
        let menu = open.then(|| self.device_menu(cx)).flatten();
        let tile = div()
            .id(("localsend-device", ix))
            .role(Role::Button)
            .aria_label(format!("Send to {name}"))
            .aria_expanded(open)
            .w(px(120.))
            .py_3()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .rounded(px(theme::RADIUS_CARD))
            .cursor_pointer()
            .hover(|s| s.bg(content(0.08)))
            .when(open, |d| d.bg(content(0.08)))
            .when(outlined, |d| d.bg(theme::accent().opacity(0.14)))
            .when(self.drag.active(), |d| {
                offers(d, move |_| drag.offer(target.clone()))
            })
            .when(open, |d| {
                d.on_hover(cx.listener(|this, over: &bool, _, _| {
                    this.localsend.on_menu_tile = *over;
                }))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                this.click_device(ix, device.clone(), window, cx)
            }))
            .child(
                div()
                    .size(px(64.))
                    .rounded_full()
                    .bg(tint.opacity(0.14))
                    .flex()
                    .items_center()
                    .justify_center()
                    // design: a device's picture, sized as AirDrop's people are.
                    .child(theme::icon(picture, 28.).text_color(tint)),
            )
            .child(
                div()
                    .w_full()
                    .px_2()
                    .text_center()
                    .truncate()
                    .text_size(px(theme::TEXT_BODY))
                    .child(name),
            )
            .child(
                div()
                    .w_full()
                    .px_2()
                    .text_center()
                    .truncate()
                    .text_size(px(theme::TEXT_CAPTION))
                    .text_color(content(0.65))
                    .child(detail),
            );
        div().relative().child(tile).children(menu.map(|menu| {
            div().absolute().left_0().top(gpui::relative(1.)).child(
                deferred(
                    gpui::anchored()
                        .anchor(gpui::Anchor::TopLeft)
                        .offset(gpui::point(px(0.), px(4.)))
                        .snap_to_window_with_margin(px(8.))
                        .child(menu),
                )
                .with_priority(1),
            )
        }))
    }
}

/// What the clipboard holds that a device can take: its files, or else its
/// text, or else its picture.
fn clipboard(cx: &App) -> Option<Clipped> {
    let files = cx
        .try_global::<Pasteboard>()
        .map(Pasteboard::files)
        .unwrap_or_default();
    if !files.is_empty() {
        return Some(Clipped::Ready(Outgoing::Files(files)));
    }
    let item = cx.read_from_clipboard()?;
    if let Some(text) = item.text().filter(|text| !text.trim().is_empty()) {
        return Some(Clipped::Ready(Outgoing::Text(text)));
    }
    item.into_entries().find_map(|entry| match entry {
        ClipboardEntry::Image(image) => Some(Clipped::Picture(image)),
        _ => None,
    })
}

/// Send Clipboard's second line: what it would send.
fn clipped_line(clipped: Option<&Clipped>) -> String {
    match clipped {
        None => "The clipboard is empty".into(),
        Some(Clipped::Picture(image)) => {
            format!("Picture, {}", format_bytes(image.bytes.len() as u64))
        }
        Some(Clipped::Ready(Outgoing::Text(text))) => one_line(text),
        Some(Clipped::Ready(outgoing)) => localsend::described(outgoing),
    }
}

/// How many rows the popover shows before it scrolls.
const HISTORY_ROWS: f32 = 6.;

/// The text devices sent, newest first: a press copies one again, and each
/// can be forgotten, or all of them.
fn history(entity: &Entity<LocalSend>, cx: &mut Context<Pane>) -> impl IntoElement + use<> {
    let messages: Vec<(u64, String, String)> = entity
        .read(cx)
        .messages
        .iter()
        .map(|message| (message.at, message.from.clone(), one_line(&message.text)))
        .collect();
    let today = chrono::Local::now().date_naive();
    let close = |pane: &mut Pane, cx: &mut Context<Pane>| {
        pane.localsend.history = false;
        cx.notify();
    };
    let rows = messages
        .into_iter()
        .enumerate()
        .map(|(ix, (at, from, line))| history_row(ix, at, from, line, today, close, cx));
    let list = if rows.len() == 0 {
        div()
            .px(px(theme::CARD_TEXT_PADDING - theme::CARD_PADDING))
            .pb_3()
            .text_size(px(theme::TEXT_SMALL))
            .text_color(content(0.65))
            .child("Text a device sends you lands here once you copy it.")
            .into_any_element()
    } else {
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("localsend-messages")
                    .max_h(px(HISTORY_ROWS * (theme::TALL_ROW_HEIGHT + theme::ROW_GAP)))
                    .overflow_y_scroll()
                    .flex()
                    .flex_col()
                    .children(rows),
            )
            .child(
                theme::row_slot("localsend-clear-history")
                    .debug_selector(|| "localsend-clear-history".into())
                    .role(Role::MenuItem)
                    .aria_label("Clear History")
                    .on_click(cx.listener(move |pane, _, _, cx| {
                        localsend::get(cx).update(cx, |localsend, cx| localsend.clear_messages(cx));
                        close(pane, cx);
                    }))
                    .child(theme::row(false).child("Clear History")),
            )
            .into_any_element()
    };
    theme::inset_rows(
        theme::floating(theme::Elevation::Popover)
            .id("localsend-history-popover")
            .debug_selector(|| "localsend-history-popover".into())
            .absolute()
            .top(px(theme::BUTTON_HEIGHT + 4.))
            .right_0()
            .w(px(320.))
            .occlude()
            .on_mouse_down_out(cx.listener(move |pane, _, _, cx| {
                if !pane.localsend.on_history_button {
                    close(pane, cx);
                }
            }))
            .flex()
            .flex_col(),
    )
    .child(theme::section_label("Received Text").mt_0())
    .child(list)
}

fn history_row(
    ix: usize,
    at: u64,
    from: String,
    line: String,
    today: chrono::NaiveDate,
    close: fn(&mut Pane, &mut Context<Pane>),
    cx: &mut Context<Pane>,
) -> impl IntoElement + use<> {
    let ink = theme::row_muted(false);
    let when = format_date(at / 1000, today);
    theme::row_slot(("localsend-message", ix))
        .debug_selector(move || format!("localsend-message-{ix}"))
        .role(Role::MenuItem)
        .aria_label(format!("Copy text from {from}"))
        .on_click(cx.listener(move |pane, _, _, cx| {
            localsend::get(cx).update(cx, |localsend, cx| localsend.copy_message(at, cx));
            close(pane, cx);
        }))
        .child(
            theme::row(false)
                .h(px(theme::TALL_ROW_HEIGHT))
                // The button's glyph, not its circle, ends on the text's edge.
                .pr_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(div().truncate().child(line))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(theme::TEXT_CAPTION))
                                .text_color(ink)
                                .child(format!("{from} · {when}")),
                        ),
                )
                .child(
                    theme::tip(
                        theme::row_icon_button(
                            ("localsend-forget", ix),
                            "Remove",
                            "cancel-01",
                            ink,
                        ),
                        "Remove",
                    )
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        localsend::get(cx)
                            .update(cx, |localsend, cx| localsend.forget_message(at, cx));
                    }),
                ),
        )
}

/// A message's start on one line, short enough to lay out at once.
fn one_line(text: &str) -> String {
    let start: String = text.chars().take(200).collect();
    start.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn send(device: Device, outgoing: Outgoing, cx: &mut gpui::App) {
    localsend::get(cx).update(cx, |localsend, cx| {
        localsend.send(device, outgoing, None, cx)
    });
}

/// Reached through Back after Settings turned LocalSend off.
fn turned_off() -> impl IntoElement {
    div()
        .id("localsend-off")
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
                .child("LocalSend is off"),
        )
        .child(
            div()
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child("Turn it on to send files to devices nearby and receive theirs."),
        )
        .child(
            div().pt_2().child(
                text_button("localsend-turn-on", "Use LocalSend", ButtonLook::Primary)
                    .on_click(|_, _, cx| preferences::update(cx, |p| p.localsend = true)),
            ),
        )
}

/// No device yet: looking, or none answered.
fn empty(scanning: bool) -> impl IntoElement {
    let (title, line) = if scanning {
        (
            "Looking for devices nearby…",
            "Phones and computers with LocalSend open on this network show here.",
        )
    } else {
        (
            "No devices found",
            "Open LocalSend on the other device, on this network, then Scan Again.",
        )
    };
    div()
        .id("localsend-empty")
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
                .text_size(px(theme::TEXT_SMALL))
                .text_color(content(0.65))
                .child(line),
        )
}

/// Whether this Mac receives, and how: the switch Settings has too.
fn footer(entity: &Entity<LocalSend>, cx: &mut Context<Pane>) -> impl IntoElement + use<> {
    let prefs = cx.global::<Preferences>();
    let on = prefs.localsend_receive;
    let folder = prefs.localsend_save_to();
    let folder = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| folder.display().to_string());
    let pin = prefs.localsend_pin.clone();
    let (said, failed) = match &entity.read(cx).receiving {
        Receiving::Off => (
            "Devices nearby can offer you files. You accept each one.".to_string(),
            false,
        ),
        Receiving::Starting => ("Starting…".to_string(), false),
        Receiving::On { .. } => {
            let pin = pin
                .map(|pin| format!(" Senders type PIN {pin}."))
                .unwrap_or_default();
            (
                format!("You accept each offer; files go to {folder}.{pin}"),
                false,
            )
        }
        Receiving::Failed(why) => (why.clone(), true),
    };
    div()
        .flex()
        .items_center()
        .gap_3()
        .px(px(theme::PANE_INSET))
        .py_3()
        .border_t_1()
        .border_color(content(0.06))
        .text_size(px(theme::TEXT_SMALL))
        .child(
            theme::switch(on)
                .id("localsend-receive")
                .role(Role::Switch)
                .aria_label("Receive files")
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .cursor_pointer()
                .on_click(move |_, _, cx| preferences::update(cx, |p| p.localsend_receive = !on)),
        )
        .child(div().flex_shrink_0().child("Receive files"))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(if failed { danger() } else { content(0.65) })
                .child(said),
        )
}

/// A circle with this Mac's initials.
fn avatar(name: &str) -> impl IntoElement + use<> {
    let initials: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    div()
        .size(px(48.))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::accent())
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme::on_accent())
        .text_size(px(theme::TEXT_TITLE))
        .font_weight(FontWeight::MEDIUM)
        .child(initials)
}

pub(super) fn kind_icon(kind: Kind) -> &'static str {
    match kind {
        Kind::Phone => "smartphone",
        Kind::Computer => "laptop",
        Kind::Browser => "globe",
        Kind::Headless => "terminal",
        Kind::Server => "server",
    }
}

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Phone => "Phone",
        Kind::Computer => "Computer",
        Kind::Browser => "Browser",
        Kind::Headless => "Command line",
        Kind::Server => "Server",
    }
}

/// A steady tint per device, so each keeps its color as others come and go.
fn tint_of(fingerprint: &str) -> Hsla {
    let tints = [
        theme::tint_blue(),
        theme::tint_purple(),
        theme::tint_pink(),
        theme::tint_orange(),
        theme::tint_indigo(),
    ];
    let hash = fingerprint.bytes().fold(5381usize, |hash, byte| {
        hash.wrapping_mul(33) ^ usize::from(byte)
    });
    tints[hash % tints.len()]
}

impl Browser {
    /// Repaints as transfers move, and says in a toast how each ended.
    pub(super) fn follow_localsend(cx: &mut Context<Self>) {
        let localsend = localsend::get(cx);
        cx.observe(&localsend, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&localsend, |this, _, said: &Said, cx| {
            let offer = said.show.clone().map(Offer::Show);
            this.toast(said.text.clone(), said.failed, offer, cx);
        })
        .detach();
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
            cx.subscribe_in(&view, window, |this, _, _: &Done, window, cx| {
                this.close_sheet(window, cx)
            })
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
        cx.subscribe_in(&view, window, |this, _, _: &Done, window, cx| {
            this.close_sheet(window, cx)
        })
        .detach();
        window.focus(&view.focus_handle(cx), cx);
        self.set_sheet(Some(Sheet::LocalSendPin(view)), cx);
    }

    /// A send or receive is under way, whose card takes the toast's place.
    pub(super) fn transferring(&self, cx: &mut Context<Self>) -> bool {
        !localsend::get(cx).read(cx).transfers.is_empty()
    }

    /// The send or receive under way, where a toast shows: who, how far,
    /// Cancel.
    pub(super) fn localsend_toast(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let entity = localsend::get(cx);
        let shown = entity.read(cx);
        let transfer = shown.transfers.first()?;
        let more = shown.transfers.len() - 1;
        let key = transfer.key;
        let peer = &transfer.peer;
        let said = match (transfer.direction, transfer.waiting) {
            (Direction::Out, true) => format!("Waiting for {peer} to accept…"),
            (Direction::Out, false) => format!("Sending to {peer}"),
            (Direction::In, _) => format!("Receiving from {peer}"),
        };
        let said = match more {
            0 => said,
            n => format!("{said}, and {n} more"),
        };
        let fraction = if transfer.total == 0 {
            0.
        } else {
            (transfer.done as f32 / transfer.total as f32).min(1.)
        };
        let progress = (!transfer.waiting).then(|| {
            format!(
                "{} of {}",
                format_bytes(transfer.done),
                format_bytes(transfer.total)
            )
        });
        let bar = if cx.global::<Preferences>().show_path_bar {
            theme::PATH_BAR_HEIGHT
        } else {
            0.
        };
        let card = theme::floating(theme::Elevation::Popover)
            .id("localsend-transfer")
            .debug_selector(|| "localsend-transfer".into())
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
            .child(div().relative().min_w_0().truncate().child(said))
            .when_some(progress, |d, progress| {
                d.child(
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
                        .child(progress),
                )
            })
            .child(
                text_button("localsend-cancel", "Cancel", ButtonLook::Ghost)
                    .debug_selector(|| "localsend-cancel".into())
                    .relative()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        localsend::get(cx).update(cx, |localsend, cx| localsend.cancel(key, cx))
                    })),
            );
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
