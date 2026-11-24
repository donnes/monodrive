//! The Settings sheet over the window: a sidebar of pages and grouped rows
//! of macOS controls, after T3 Code's settings modal and Monocode's pages.
//! Every control writes straight to [`Preferences`], so changes apply live.

mod agents_page;
mod appearance;
mod general;
mod keyboard;
mod window;

use crate::agents::{self, Action, Harness, Setup};
use crate::app_icon::{AppIcon, Picture};
use crate::background::{self, Backdrop, Effect, Look, Scope};
use crate::commands::CheckForUpdates;
use crate::commands::{self, Group};
use crate::finder::{self, Viewer};
use crate::frame;
use crate::motion;
use crate::native::native;
use crate::palettes::{self, PALETTES};
use crate::preferences::{
    self, GRID_ICON_SIZES, Preferences, RowSize, Theme, UI_SCALE_PERCENTS, View, WORKTREE_LIMITS,
    WindowSize, WorkspaceStyle,
};
use crate::theme::{
    self, ButtonLook, Elevation, accent, content, focus_ring, icon, icon_button, knob, knob_shadow,
    on_accent, text_button,
};
use crate::workspace::Terminal;
use gpui::{
    Anchor, AnimationExt, AnyElement, App, Bounds, ClipboardItem, Context, Div, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels, PromptLevel, Role, ScrollHandle,
    SharedString, Stateful, Subscription, Toggled, Window, actions, anchored, canvas, deferred,
    div, img, point, prelude::*, px, relative,
};
use monodrive_core::entries::SortBy;
use monodrive_core::mcp::{Profile, TOOLS};
use monodrive_core::updates::Channel;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

actions!(settings, [Close, FocusNext, FocusPrevious]);

pub fn bind(cx: &mut App) {
    let context = Some("Settings");
    cx.bind_keys([
        KeyBinding::new("escape", Close, context),
        KeyBinding::new("tab", FocusNext, context),
        KeyBinding::new("shift-tab", FocusPrevious, context),
    ]);
}

/// Controls outside the scrolling page, which never need scrolling to.
const FIXED_CONTROLS: [&str; 7] = [
    "nav-General",
    "nav-Window",
    "nav-Appearance",
    "nav-Keyboard",
    "nav-Agents",
    "restore-defaults",
    "close",
];

/// Asks the owner to close the sheet.
pub struct Dismissed;

const NAV_WIDTH: f32 = 200.;
const KNOB: f32 = 16.;

/// Monocode's accent presets; the theme's own accent comes first.
const ACCENTS: [(u32, &str); 6] = [
    (0x4da3f5, "Blue"),
    (0x8b5cf6, "Violet"),
    (0xec4899, "Pink"),
    (0xef4444, "Red"),
    (0xf59e0b, "Orange"),
    (0x10b981, "Green"),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    General,
    Window,
    Appearance,
    Keyboard,
    Agents,
    Notifications,
}

impl Page {
    const ALL: [Page; 6] = [
        Page::General,
        Page::Window,
        Page::Appearance,
        Page::Keyboard,
        Page::Agents,
        Page::Notifications,
    ];

    fn label(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Window => "Window",
            Page::Appearance => "Appearance",
            Page::Notifications => "Notifications",
            Page::Keyboard => "Keyboard",
            Page::Agents => "Agents",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::General => "sliders",
            Page::Window => "app-window",
            Page::Appearance => "palette",
            Page::Notifications => "bell",
            Page::Keyboard => "keyboard",
            Page::Agents => "bot",
        }
    }

    fn nav_id(self) -> &'static str {
        match self {
            Page::General => "nav-General",
            Page::Window => "nav-Window",
            Page::Appearance => "nav-Appearance",
            Page::Notifications => "nav-Notifications",
            Page::Keyboard => "nav-Keyboard",
            Page::Agents => "nav-Agents",
        }
    }

    /// What its Restore Defaults puts back, if it has one.
    fn restore(self) -> Option<fn(&mut Preferences)> {
        match self {
            Page::General => Some(Preferences::restore_general),
            Page::Window => Some(Preferences::restore_window),
            Page::Appearance => Some(Preferences::restore_appearance),
            Page::Notifications => Some(Preferences::restore_notifications),
            Page::Keyboard => Some(|p| p.shortcuts.clear()),
            Page::Agents => Some(|p| {
                p.agents_change_files = true;
                p.agents_all_tools = false;
            }),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Slider {
    Hue,
    Saturation,
    DarkLightness,
    SidebarOpacity,
    BackgroundEmpty,
    BackgroundFolder,
    GridIcon,
}

impl Slider {
    fn id(self) -> &'static str {
        match self {
            Slider::Hue => "hue",
            Slider::Saturation => "saturation",
            Slider::DarkLightness => "dark-lightness",
            Slider::SidebarOpacity => "sidebar-opacity",
            Slider::BackgroundEmpty => "background-empty",
            Slider::BackgroundFolder => "background-folder",
            Slider::GridIcon => "grid-icon",
        }
    }

    fn range(self) -> (u32, u32) {
        match self {
            Slider::Hue => (0, 360),
            Slider::Saturation => (0, 100),
            Slider::DarkLightness => (0, 30),
            Slider::SidebarOpacity => (15, 100),
            Slider::BackgroundEmpty | Slider::BackgroundFolder => (5, 65),
            Slider::GridIcon => (*GRID_ICON_SIZES.start(), *GRID_ICON_SIZES.end()),
        }
    }

    fn get(self, p: &Preferences) -> u32 {
        match self {
            Slider::Hue => p.hue,
            Slider::Saturation => p.saturation,
            Slider::DarkLightness => p.dark_lightness,
            Slider::SidebarOpacity => p.sidebar_opacity,
            Slider::BackgroundEmpty => p.background_empty,
            Slider::BackgroundFolder => p.background_folder,
            Slider::GridIcon => p.grid_icon,
        }
    }

    fn set(self, p: &mut Preferences, value: u32) {
        match self {
            Slider::Hue => p.hue = value,
            Slider::Saturation => p.saturation = value,
            Slider::DarkLightness => p.dark_lightness = value,
            Slider::SidebarOpacity => p.sidebar_opacity = value,
            Slider::BackgroundEmpty => p.background_empty = value,
            Slider::BackgroundFolder => p.background_folder = value,
            Slider::GridIcon => p.grid_icon = value,
        }
    }

    fn display(self, value: u32) -> String {
        match self {
            Slider::Hue => format!("{value}°"),
            Slider::GridIcon => format!("{value} pt"),
            _ => format!("{value}%"),
        }
    }
}

pub struct SettingsView {
    /// The terminals on offer.
    terminals: Vec<Terminal>,
    /// Who gets other apps' Show in Finder, once macOS has said.
    finder: Option<finder::Status>,
    focus: FocusHandle,
    page: Page,
    /// Each slider's track as last laid out, to turn a pointer into a value.
    tracks: Rc<RefCell<HashMap<Slider, Bounds<Pixels>>>>,
    dragging: Option<Slider>,
    /// The pop-up menu that is open, by id.
    menu: Option<&'static str>,
    /// The open menu's highlighted item, moved by the arrows or the pointer.
    menu_active: usize,
    menu_focus: FocusHandle,
    /// The copy button last pressed, which reads Copied.
    copied: Option<&'static str>,
    /// The agent the Agents page shows setup for, and those whose config
    /// already has Monodrive.
    harness: Harness,
    /// The agents that look installed, the only ones offered. `None` until read.
    installed: Option<Vec<Harness>>,
    added: Vec<Harness>,
    /// The Add or Remove last pressed, while it runs and after it fails.
    job: Option<Job>,
    /// Whether the pointer came onto the Add button since it last changed;
    /// an added agent's button reads Remove only then, so a second click
    /// on Add can't take it straight back out.
    over_action: bool,
    /// Whether "Do it yourself" shows the manual steps.
    by_hand: bool,
    /// The command whose shortcut is being recorded, and why the last key
    /// pressed for it was refused.
    recording: Option<&'static str>,
    recording_error: Option<String>,
    recorder: Option<Subscription>,
    preview: Entity<Backdrop>,
    /// Each control's focus, by id, so Tab and focus survive re-renders.
    handles: RefCell<HashMap<SharedString, FocusHandle>>,
    /// Where each control was last laid out, to scroll the focused one into view.
    probes: Rc<RefCell<HashMap<SharedString, Bounds<Pixels>>>>,
    page_scroll: ScrollHandle,
}

/// An Add or Remove on one agent's config.
struct Job {
    harness: Harness,
    action: Action,
    /// Why it failed, once it has; it's running until then.
    failed: Option<String>,
}

/// One choice in a pop-up menu.
struct MenuItem {
    label: SharedString,
    /// Drawn in its row, and again on the button when it's the one chosen.
    icon: Option<Rc<dyn Fn() -> AnyElement>>,
    selected: bool,
    choose: Choose,
}

/// What picking a menu item does.
type Choose = Rc<dyn Fn(&mut App)>;

impl EventEmitter<Dismissed> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::find_terminals(cx);
        Self::ask_finder(None, cx);
        Self {
            terminals: vec![Terminal::Apple],
            finder: None,
            focus: cx.focus_handle(),
            page: Page::General,
            tracks: Rc::default(),
            dragging: None,
            menu: None,
            menu_active: 0,
            menu_focus: cx.focus_handle(),
            copied: None,
            harness: Harness::ClaudeCode,
            installed: None,
            added: Vec::new(),
            job: None,
            over_action: false,
            by_hand: false,
            recording: None,
            recording_error: None,
            recorder: None,
            preview: cx.new(|_| Backdrop::default()),
            handles: RefCell::default(),
            probes: Rc::default(),
            page_scroll: ScrollHandle::new(),
        }
    }

    /// Esc closes an open menu first, then the sheet.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.menu.take() {
            Some(id) => self.close_menu(id, window, cx),
            None => cx.emit(Dismissed),
        }
    }

    /// Opens a pop-up menu on its chosen item and gives it the keys.
    fn open_menu(
        &mut self,
        id: &'static str,
        selected: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = Some(id);
        self.menu_active = selected;
        window.focus(&self.menu_focus, cx);
        cx.notify();
    }

    /// Tab out of an open menu closes it and moves on from its button.
    fn leave_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.menu {
            self.close_menu(id, window, cx);
        }
    }

    /// Returns focus to the menu's button.
    fn close_menu(&mut self, id: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        window.focus(&self.handle(id, cx), cx);
        cx.notify();
    }

    fn handle(&self, id: impl Into<SharedString>, cx: &App) -> FocusHandle {
        self.handles
            .borrow_mut()
            .entry(id.into())
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Makes `control` a Tab stop with a keyboard focus ring. A disabled
    /// control is skipped. Enter and Space press a focused control's
    /// `on_click`, as GPUI does for any focused element.
    fn focusable(
        &self,
        id: impl Into<SharedString>,
        enabled: bool,
        control: Stateful<Div>,
        cx: &App,
    ) -> Stateful<Div> {
        let id = id.into();
        let handle = self.handle(id.clone(), cx).tab_stop(enabled);
        let probes = self.probes.clone();
        control
            .relative()
            .track_focus(&handle)
            .focus_visible(|s| s.shadow(focus_ring()))
            .child(
                canvas(
                    move |bounds, _, _| {
                        probes.borrow_mut().insert(id.clone(), bounds);
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }

    /// Scrolls the page so the focused control shows, after Tab moved to it.
    fn reveal(&self, window: &Window) {
        let focused = self
            .handles
            .borrow()
            .iter()
            .find(|(id, handle)| {
                !FIXED_CONTROLS.contains(&id.as_ref()) && handle.is_focused(window)
            })
            .map(|(id, _)| id.clone());
        let Some(bounds) = focused.and_then(|id| self.probes.borrow().get(&id).copied()) else {
            return;
        };
        let view = self.page_scroll.bounds();
        let margin = px(24.);
        let mut offset = self.page_scroll.offset();
        if bounds.top() < view.top() + margin {
            offset.y += view.top() + margin - bounds.top();
        } else if bounds.bottom() > view.bottom() - margin {
            offset.y -= bounds.bottom() - (view.bottom() - margin);
        } else {
            return;
        }
        offset.y = offset.y.clamp(-self.page_scroll.max_offset().y, px(0.));
        self.page_scroll.set_offset(offset);
    }

    fn show(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.menu = None;
        self.copied = None;
        self.recording = None;
        self.recording_error = None;
        self.recorder = None;
        if page == Page::Agents {
            self.check_agents(cx);
        }
        cx.notify();
    }

    /// Who gets other apps' Show in Finder, after switching it `on` or off
    /// if asked. Off the UI thread, as LaunchServices names the app.
    fn ask_finder(switch: Option<bool>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let status = cx
                .background_executor()
                .spawn(async move { switch.map_or_else(finder::status, finder::set) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finder = Some(status);
                cx.notify();
            });
        })
        .detach();
    }

    /// Which terminals are installed, asked off the UI thread. The one chosen
    /// stays on offer, so the control always shows what's set.
    fn find_terminals(cx: &mut Context<Self>) {
        let native = native(cx);
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    Terminal::ALL
                        .into_iter()
                        .filter(|terminal| native.installed(terminal.bundle_id()))
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let chosen = cx.global::<Preferences>().terminal;
                this.terminals = Terminal::ALL
                    .into_iter()
                    .filter(|terminal| {
                        *terminal == chosen
                            || *terminal == Terminal::Apple
                            || found.contains(terminal)
                    })
                    .collect();
                cx.notify();
            });
        })
        .detach();
    }

    fn slide(&mut self, kind: Slider, x: Pixels, cx: &mut Context<Self>) {
        let Some(track) = self.tracks.borrow().get(&kind).copied() else {
            return;
        };
        let value = slider_value(
            kind.range(),
            (x - track.left()).as_f32(),
            track.size.width.as_f32(),
        );
        preferences::update(cx, |p| kind.set(p, value));
    }

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(NAV_WIDTH))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(theme::ROW_GAP))
            // Its rows' corners run parallel to the sheet's.
            .p(px(theme::CARD_PADDING))
            .border_r_1()
            .border_color(content(0.06))
            // ↑ and ↓ switch pages from the page list, as in System Settings.
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let Some(step) = arrow(&event.keystroke.key) else {
                    return;
                };
                let at = Page::ALL
                    .iter()
                    .position(|page| *page == this.page)
                    .unwrap_or(0);
                let page = Page::ALL[at.saturating_add_signed(step).min(Page::ALL.len() - 1)];
                this.show(page, cx);
                window.focus(&this.handle(page.nav_id(), cx), cx);
                cx.stop_propagation();
            }))
            .children(Page::ALL.map(|page| {
                let active = page == self.page;
                let item = div()
                    .id(page.label())
                    .debug_selector(move || page.nav_id().into())
                    .role(Role::Tab)
                    .aria_label(page.label())
                    .aria_selected(active)
                    .h(px(theme::ROW_HEIGHT))
                    .px_2()
                    .rounded(px(theme::RADIUS_ROW))
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(theme::TEXT_BODY))
                    .cursor_pointer()
                    .map(|d| {
                        if active {
                            d.bg(content(0.14)).text_color(content(1.))
                        } else {
                            d.text_color(content(0.65))
                                .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                        }
                    })
                    .child(icon(page.icon(), 16.).text_color(content(if active {
                        1.
                    } else {
                        0.55
                    })))
                    .child(page.label())
                    .on_click(cx.listener(move |this, _, _, cx| this.show(page, cx)));
                self.focusable(page.nav_id(), true, item, cx)
            }))
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let page = self.page;
        div()
            .h(px(theme::TITLE_BAR_HEIGHT))
            .flex_shrink_0()
            .px_4()
            .flex()
            .items_center()
            .gap_2()
            .border_b_1()
            .border_color(content(0.06))
            .text_size(px(theme::TEXT_BODY))
            .child(div().text_color(content(0.65)).child("Settings"))
            .child(div().text_color(content(0.3)).child("/"))
            .child(
                div()
                    .flex_1()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.page.label()),
            )
            .children(page.restore().map(|restore| {
                self.focusable(
                    "restore-defaults",
                    true,
                    theme::button("restore-defaults", ButtonLook::Ghost)
                        .aria_label("Restore defaults")
                        .group("restore-defaults")
                        .child(
                            icon("rotate-ccw", 12.)
                                .text_color(content(0.55))
                                .group_hover("restore-defaults", |s| s.text_color(content(1.))),
                        )
                        .child("Restore Defaults")
                        .on_click(move |_, _, cx| preferences::update(cx, restore)),
                    cx,
                )
            }))
            .child(
                self.focusable(
                    "close",
                    true,
                    theme::tip(
                        icon_button("close-settings", "Close", "cancel-01", true),
                        "Close",
                    )
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
                    cx,
                ),
            )
    }

    /// A pop-up button and its menu, as AppKit draws them: the current item
    /// on a capsule, the choices in a glass menu with an accent highlight.
    fn popup(
        &self,
        id: &'static str,
        label: &'static str,
        width: f32,
        items: Vec<MenuItem>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open = self.menu == Some(id);
        let selected = items.iter().position(|item| item.selected).unwrap_or(0);
        let chooses: Vec<Choose> = items.iter().map(|item| item.choose.clone()).collect();
        let active = self.menu_active;
        let (current, current_icon) = items
            .iter()
            .position(|item| item.selected)
            .map(|ix| {
                let item = &items[ix];
                (item.label.clone(), item.icon.is_some())
            })
            .unwrap_or_default();
        let trigger_icon = current_icon
            .then(|| items.iter().find(|item| item.selected))
            .flatten()
            .and_then(|item| item.icon.as_ref().map(|icon| icon()));
        let menu = open.then(|| {
            let rows = items.into_iter().enumerate().map(|(ix, item)| {
                let MenuItem {
                    label,
                    icon: item_icon,
                    selected,
                    choose,
                } = item;
                theme::row_slot((id, ix))
                    .role(Role::MenuItemRadio)
                    .aria_label(label.clone())
                    .aria_selected(selected)
                    .child(
                        theme::row(ix == active)
                            .child(theme::lead(selected.then(|| {
                                icon("check-hi", 12.).text_color(if ix == active {
                                    on_accent()
                                } else {
                                    content(1.)
                                })
                            })))
                            .children(item_icon.map(|icon| icon()))
                            .child(label),
                    )
                    // The pointer and the arrows share one highlight, as in AppKit menus.
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if this.menu_active != ix {
                            this.menu_active = ix;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        choose(cx);
                        this.close_menu(id, window, cx);
                    }))
            });
            let viewport = window.viewport_size();
            div()
                .child(
                    // Catches the press outside the menu that closes it.
                    deferred(
                        anchored().position(point(px(0.), px(0.))).child(
                            div()
                                .id("menu-dismiss")
                                .w(viewport.width)
                                .h(viewport.height)
                                .occlude()
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        this.close_menu(id, window, cx);
                                        cx.stop_propagation();
                                    }),
                                ),
                        ),
                    )
                    .with_priority(1),
                )
                .child(
                    div().absolute().right_0().top(relative(1.)).child(
                        deferred(
                            anchored()
                                .anchor(Anchor::TopRight)
                                .offset(point(px(0.), px(4.)))
                                .snap_to_window_with_margin(px(8.))
                                .child(
                                    theme::floating(Elevation::Popover)
                                        .id("menu")
                                        .role(Role::Menu)
                                        .aria_label(label)
                                        .track_focus(&self.menu_focus)
                                        .on_key_down(cx.listener(
                                            move |this, event: &KeyDownEvent, window, cx| {
                                                let last = chooses.len().saturating_sub(1);
                                                match event.keystroke.key.as_str() {
                                                    "up" => {
                                                        this.menu_active =
                                                            this.menu_active.saturating_sub(1)
                                                    }
                                                    "down" => {
                                                        this.menu_active =
                                                            (this.menu_active + 1).min(last)
                                                    }
                                                    "enter" | "space" => {
                                                        if let Some(choose) =
                                                            chooses.get(this.menu_active)
                                                        {
                                                            choose(cx);
                                                        }
                                                        this.close_menu(id, window, cx);
                                                    }
                                                    _ => return,
                                                }
                                                cx.stop_propagation();
                                                cx.notify();
                                            },
                                        ))
                                        .occlude()
                                        .min_w(px(width.max(120.)))
                                        .max_h(px(360.))
                                        .child(
                                            theme::inset_rows(div())
                                                .id("menu-items")
                                                .relative()
                                                .max_h(px(358.))
                                                .overflow_y_scroll()
                                                .flex()
                                                .flex_col()
                                                .children(rows),
                                        ),
                                ),
                        )
                        .with_priority(2),
                    ),
                )
        });
        div()
            .relative()
            .child(
                self.focusable(
                    id,
                    true,
                    theme::button(id, ButtonLook::Secondary)
                        .role(Role::ComboBox)
                        .aria_label(format!("{label}: {current}"))
                        .aria_expanded(open)
                        .min_w(px(width))
                        .pl(px(if trigger_icon.is_some() { 8. } else { 12. }))
                        .pr(px(6.))
                        .gap(px(6.))
                        .children(trigger_icon)
                        .child(div().flex_1().child(current))
                        .child(icon("chevron-down-hi", 12.).text_color(content(0.55)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if this.menu == Some(id) {
                                this.close_menu(id, window, cx);
                            } else {
                                this.open_menu(id, selected, window, cx);
                            }
                        }))
                        // ↓ opens the menu too, as on a macOS pop-up button.
                        .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "down" && this.menu != Some(id) {
                                this.open_menu(id, selected, window, cx);
                                cx.stop_propagation();
                            }
                        })),
                    cx,
                ),
            )
            .children(menu)
            .into_any_element()
    }

    /// A slider with an accent fill up to its knob, then its value.
    fn slider(
        &self,
        kind: Slider,
        p: &Preferences,
        disabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (min, max) = kind.range();
        let value = kind.get(p).clamp(min, max);
        let fraction = (value - min) as f32 / (max - min) as f32;
        let tracks = self.tracks.clone();
        div()
            .w(px(220.))
            .flex()
            .items_center()
            .gap_3()
            .when(disabled, |d| d.opacity(0.4))
            .child(
                self.focusable(
                    kind.id(),
                    !disabled,
                    div()
                        .id(kind.id())
                        .role(Role::Slider)
                        .aria_label(kind.id())
                        // The arrows nudge it by one, or by ten with Shift.
                        .on_key_down(move |event: &KeyDownEvent, _, cx| {
                            let Some(step) = arrow(&event.keystroke.key) else {
                                return;
                            };
                            let step = if event.keystroke.modifiers.shift {
                                step * 10
                            } else {
                                step
                            };
                            let next = (value as i64 + step as i64).clamp(min as i64, max as i64);
                            preferences::update(cx, |p| kind.set(p, next as u32));
                            cx.stop_propagation();
                        })
                        .flex_1()
                        .h(px(KNOB))
                        .relative()
                        .flex()
                        .items_center()
                        .child(
                            canvas(
                                move |bounds, _, _| {
                                    tracks.borrow_mut().insert(kind, bounds);
                                },
                                |_, _, _, _| {},
                            )
                            .absolute()
                            .size_full(),
                        )
                        .child(theme::progress_track().w_full())
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top(px((KNOB - 4.) / 2.))
                                .h(px(4.))
                                .w(relative(fraction))
                                .rounded_full()
                                .bg(accent()),
                        )
                        .child(
                            // The knob travels the track less its own width.
                            div()
                                .absolute()
                                .top_0()
                                .left(relative(fraction))
                                .ml(px(-KNOB * fraction))
                                .size(px(KNOB))
                                .rounded_full()
                                .bg(knob())
                                .shadow(knob_shadow()),
                        )
                        .when(!disabled, |d| {
                            d.cursor_pointer().on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.dragging = Some(kind);
                                    this.slide(kind, event.position.x, cx);
                                    cx.stop_propagation();
                                }),
                            )
                        }),
                    cx,
                ),
            )
            .child(
                div()
                    .w(px(36.))
                    .flex_shrink_0()
                    .text_right()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(content(0.65))
                    .child(kind.display(value)),
            )
            .into_any_element()
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A focused control that went away leaves keys with nowhere to go;
        // the sheet takes them back so Tab and Esc keep working. The check
        // reads the last frame, so it runs again once this one is drawn, in
        // case a replacement control took the focus.
        if !self.focus.contains_focused(window, cx) && !self.menu_focus.is_focused(window) {
            let (focus, menu) = (self.focus.clone(), self.menu_focus.clone());
            window.on_next_frame(move |window, cx| {
                if !focus.contains_focused(window, cx) && !menu.is_focused(window) {
                    window.focus(&focus, cx);
                }
            });
        }
        let prefs = cx.global::<Preferences>().clone();
        let groups = match self.page {
            Page::General => self.general(&prefs, cx),
            Page::Window => self.window_page(&prefs, cx),
            Page::Appearance => self.appearance(&prefs, window, cx),
            Page::Notifications => self.notifications(&prefs, window, cx),
            Page::Keyboard => self.keyboard(&prefs, cx),
            Page::Agents => self.agents(&prefs, window, cx),
        };
        let card = theme::floating(Elevation::Sheet)
            .id("settings")
            .role(Role::Dialog)
            .aria_label("Settings")
            .size_full()
            .max_w(px(880.))
            .max_h(px(640.))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .relative()
                    .size_full()
                    .flex()
                    .child(self.nav(cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(self.header(cx))
                            .child(
                                div()
                                    .id("settings-page")
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scroll()
                                    .track_scroll(&self.page_scroll)
                                    .child(
                                        div().flex().justify_center().child(
                                            div()
                                                .w_full()
                                                .max_w(px(620.))
                                                .px_6()
                                                .py_6()
                                                .flex()
                                                .flex_col()
                                                .gap_6()
                                                .children(groups),
                                        ),
                                    ),
                            ),
                    ),
            );
        let card = motion::rise_in(card, "settings-enter");
        div()
            .id("settings-overlay")
            // A click anywhere ends a recording, so it can't be left running.
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| {
                if this.recording.is_some() {
                    this.stop_recording(cx);
                }
            }))
            .key_context("Settings")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Close, window, cx| this.close(window, cx)))
            .on_action(cx.listener(|this, _: &FocusNext, window, cx| {
                this.leave_menu(window, cx);
                window.focus_next(cx);
                this.reveal(window);
            }))
            .on_action(cx.listener(|this, _: &FocusPrevious, window, cx| {
                this.leave_menu(window, cx);
                window.focus_prev(cx);
                this.reveal(window);
            }))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .items_center()
            .justify_center()
            .p_6()
            .pt(px(theme::TITLE_BAR_HEIGHT))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let Some(kind) = this.dragging {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.slide(kind, event.position.x, cx);
                    } else {
                        this.dragging = None;
                    }
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.dragging = None),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(Dismissed)),
            )
            .child(card)
            .with_animation("settings-scrim", motion::entrance(), |scrim, t| {
                scrim.bg(theme::scrim(t))
            })
    }
}

/// Six digits, as LocalSend's PINs usually are.
fn new_pin() -> String {
    use std::hash::BuildHasher;
    let random =
        std::collections::hash_map::RandomState::new().hash_one(std::time::SystemTime::now());
    format!("{:06}", random % 1_000_000)
}

/// The value at `offset` points along a track `width` wide. The knob's
/// center can't pass the track's ends, so neither can the value.
fn slider_value((min, max): (u32, u32), offset: f32, width: f32) -> u32 {
    let travel = (width - KNOB).max(1.);
    let fraction = ((offset - KNOB / 2.) / travel).clamp(0., 1.);
    min + ((max - min) as f32 * fraction).round() as u32
}

/// A titled section of rows on a rounded plate, as System Settings groups them.
fn group(
    title: &'static str,
    description: Option<&'static str>,
    rows: Vec<AnyElement>,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                // Over the plate's text, past the plate's border.
                .px(px(theme::CARD_TEXT_PADDING + 1.))
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(px(theme::TEXT_BODY))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .children(description.map(|text| {
                    div()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(text)
                })),
        )
        .child(
            div()
                .rounded(px(theme::RADIUS_CARD))
                .bg(content(0.03))
                .border_1()
                .border_color(content(0.06))
                .flex()
                .flex_col()
                .children(rows.into_iter().enumerate().map(|(ix, row)| {
                    div()
                        .mx(px(theme::CARD_TEXT_PADDING))
                        .when(ix > 0, |d| d.border_t_1().border_color(content(0.06)))
                        .child(row)
                })),
        )
        .into_any_element()
}

/// A label and its description beside one control.
fn row(
    label: &'static str,
    description: Option<&'static str>,
    control: impl IntoElement,
) -> AnyElement {
    row_with(label, description.map(SharedString::from), control)
}

/// A row whose description changes.
fn row_with(
    label: impl Into<SharedString>,
    description: Option<SharedString>,
    control: impl IntoElement,
) -> AnyElement {
    div()
        .min_h(px(44.))
        .py(px(10.))
        .flex()
        .items_center()
        .gap_6()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(theme::TEXT_BODY)).child(label.into()))
                .children(description.map(|text| {
                    div()
                        .text_size(px(theme::TEXT_CAPTION))
                        .line_height(px(16.))
                        .text_color(content(0.65))
                        .child(text)
                })),
        )
        .child(div().flex_shrink_0().child(control))
        .into_any_element()
}

fn toggle(
    id: &'static str,
    label: &'static str,
    on: bool,
    set: fn(&mut Preferences, bool),
) -> Stateful<Div> {
    theme::switch(on)
        .id(id)
        .role(Role::Switch)
        .aria_label(label)
        .aria_toggled(if on { Toggled::True } else { Toggled::False })
        .cursor_pointer()
        .on_click(move |_, _, cx| preferences::update(cx, |p| set(p, !on)))
}

/// Capsule segments in a capsule track, as macOS draws a segmented picker.
fn segmented<T: Copy + PartialEq + 'static>(
    id: &'static str,
    label: &'static str,
    value: T,
    options: &[(T, &'static str)],
    set: fn(&mut Preferences, T),
) -> Stateful<Div> {
    let values: Vec<T> = options.iter().map(|(option, _)| *option).collect();
    let at = values
        .iter()
        .position(|option| *option == value)
        .unwrap_or(0);
    div()
        .id(id)
        .role(Role::RadioGroup)
        .aria_label(label)
        // One Tab stop; the arrows pick, as in a macOS radio group.
        .on_key_down(move |event: &KeyDownEvent, _, cx| {
            if let Some(step) = arrow(&event.keystroke.key) {
                let next = values[at.saturating_add_signed(step).min(values.len() - 1)];
                preferences::update(cx, |p| set(p, next));
                cx.stop_propagation();
            }
        })
        .flex()
        .p(px(2.))
        .gap(px(2.))
        .rounded_full()
        .bg(content(0.08))
        .children(options.iter().enumerate().map(|(ix, &(option, text))| {
            let selected = option == value;
            div()
                .id((id, ix))
                .role(Role::RadioButton)
                .aria_label(text)
                .aria_selected(selected)
                .h(px(theme::BUTTON_HEIGHT - 4.))
                .px_3()
                .rounded_full()
                .flex()
                .items_center()
                .text_size(px(theme::TEXT_SMALL))
                .map(|d| {
                    if selected {
                        d.bg(theme::raised())
                            .shadow(theme::raised_shadow())
                            .font_weight(FontWeight::MEDIUM)
                    } else {
                        d.text_color(content(0.65))
                            .cursor_pointer()
                            .hover(|s| s.text_color(content(1.)))
                    }
                })
                .child(text)
                .on_click(move |_, _, cx| preferences::update(cx, |p| set(p, option)))
        }))
}

/// The theme's accent, then Monocode's presets, as System Settings' row of
/// accent dots.
fn accent_picker(p: &Preferences, light: bool) -> Stateful<Div> {
    let theme_accent = palettes::palette(&p.palette, light).accent;
    let choices: Vec<_> = std::iter::once((None, theme_accent, "Theme"))
        .chain(
            ACCENTS
                .iter()
                .map(|&(color, label)| (Some(color), color, label)),
        )
        .collect();
    let values: Vec<Option<u32>> = choices.iter().map(|(value, _, _)| *value).collect();
    let at = values
        .iter()
        .position(|value| *value == p.accent)
        .unwrap_or(0);
    div()
        .id("accent")
        .role(Role::RadioGroup)
        .aria_label("Accent color")
        .on_key_down(move |event: &KeyDownEvent, _, cx| {
            if let Some(step) = arrow(&event.keystroke.key) {
                let next = values[at.saturating_add_signed(step).min(values.len() - 1)];
                preferences::update(cx, |p| p.accent = next);
                cx.stop_propagation();
            }
        })
        .rounded_full()
        .flex()
        .items_center()
        .gap(px(8.))
        .children(
            choices
                .into_iter()
                .enumerate()
                .map(|(ix, (value, color, label))| {
                    let selected = p.accent == value;
                    div()
                        .id(("accent", ix))
                        .role(Role::RadioButton)
                        .aria_label(label)
                        .aria_selected(selected)
                        .size(px(18.))
                        .rounded_full()
                        // design: a swatch shows the color it offers.
                        .bg(gpui::rgb(color))
                        .border_1()
                        .border_color(content(0.2))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .when(selected, |d| {
                            d.child(div().size(px(6.)).rounded_full().bg(knob()))
                        })
                        .on_click(move |_, _, cx| preferences::update(cx, |p| p.accent = value))
                }),
        )
}

/// The icons on offer, as the pictures the Dock will show. With no choice,
/// the one this build came with is selected.
fn app_icon_picker(chosen: Option<AppIcon>, window: &mut Window, cx: &mut App) -> Stateful<Div> {
    const SIDE: f32 = 76.;
    // The pictures keep macOS's icon margin, 100 of every 1024 points on each
    // side; the icons' own edges line up with the plate's text instead.
    let margin = SIDE * 100. / 1024.;
    let shown = chosen.or(AppIcon::bundled());
    let at = shown.and_then(|shown| AppIcon::ALL.iter().position(|icon| *icon == shown));
    div()
        .id("app-icon")
        .role(Role::RadioGroup)
        .aria_label("App icon")
        .on_key_down(move |event: &KeyDownEvent, _, cx| {
            if let Some(step) = arrow(&event.keystroke.key) {
                let next = at.map_or(0, |at| {
                    at.saturating_add_signed(step).min(AppIcon::ALL.len() - 1)
                });
                preferences::update(cx, |p| p.app_icon = Some(AppIcon::ALL[next]));
                cx.stop_propagation();
            }
        })
        .mx(px(-margin))
        .pt(px(12. - margin))
        .pb(px(10.))
        .rounded(px(theme::RADIUS_CARD))
        .flex()
        .flex_wrap()
        .justify_between()
        .gap(px(8.))
        .children(AppIcon::ALL.into_iter().enumerate().map(|(ix, choice)| {
            let selected = shown == Some(choice);
            let picture = Picture {
                icon: choice,
                pixels: (SIDE * window.scale_factor()).ceil() as u32,
            };
            let picture = window.use_asset::<Picture>(&picture, cx).flatten();
            div()
                .id(("app-icon", ix))
                .role(Role::RadioButton)
                .aria_label(choice.label())
                .aria_selected(selected)
                .flex()
                .flex_col()
                .items_center()
                .cursor_pointer()
                .child(match picture {
                    Some(picture) => img(picture).size(px(SIDE)).into_any_element(),
                    None => div().size(px(SIDE)).into_any_element(),
                })
                .child(
                    div()
                        .mt(px(6. - margin))
                        .text_size(px(theme::TEXT_SMALL))
                        .map(|d| {
                            if selected {
                                d.font_weight(FontWeight::MEDIUM).text_color(accent())
                            } else {
                                d.text_color(content(0.65))
                            }
                        })
                        .child(choice.label()),
                )
                .when(!selected, |d| d.hover(|s| s.text_color(content(1.))))
                .on_click(move |_, _, cx| preferences::update(cx, |p| p.app_icon = Some(choice)))
        }))
}

/// The step an arrow key takes through a group: back for ← and ↑.
fn arrow(key: &str) -> Option<isize> {
    match key {
        "left" | "up" => Some(-1),
        "right" | "down" => Some(1),
        _ => None,
    }
}

/// A preset's surface with its accent dot, at a menu icon's size.
fn swatch(surface: u32, accent: u32) -> impl IntoElement {
    div()
        .size(px(14.))
        .flex_shrink_0()
        .rounded(px(theme::RADIUS_THUMB))
        .border_1()
        .border_color(content(0.2))
        // design: a swatch shows the preset's own colors.
        .bg(gpui::rgb(surface))
        .flex()
        .items_center()
        .justify_center()
        // design: a swatch shows the preset's own colors.
        .child(div().size(px(6.)).rounded_full().bg(gpui::rgb(accent)))
}

/// A numbered step of a setup, with what it shows below its text.
fn step_row(number: u32, text: String, below: Option<AnyElement>) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .size(px(18.))
                .flex_shrink_0()
                .rounded_full()
                .bg(content(0.08))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(theme::TEXT_CAPTION))
                .text_color(content(0.65))
                .child(number.to_string()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .pt(px(1.))
                        .text_size(px(theme::TEXT_SMALL))
                        .line_height(px(16.))
                        .child(text),
                )
                .children(below),
        )
}

/// The green dot by an agent whose config already has Monodrive.
/// What stays picked once the list is known: `picked` if it's there, else the
/// first.
fn keep_or_first(picked: Harness, installed: &[Harness]) -> Harness {
    match installed.first() {
        Some(first) if !installed.contains(&picked) => *first,
        _ => picked,
    }
}

fn added_dot() -> impl IntoElement {
    div()
        .size(px(6.))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_picked_agent_stays_only_if_it_is_installed() {
        let installed = [Harness::Codex, Harness::Cursor];
        assert_eq!(keep_or_first(Harness::Cursor, &installed), Harness::Cursor);
        assert_eq!(
            keep_or_first(Harness::ClaudeCode, &installed),
            Harness::Codex
        );
        assert_eq!(keep_or_first(Harness::ClaudeCode, &[]), Harness::ClaudeCode);
    }

    #[test]
    fn slider_ends_reach_the_range_ends_and_clamp_beyond() {
        let range = (15, 100);
        let width = 216.;
        assert_eq!(slider_value(range, KNOB / 2., width), 15);
        assert_eq!(slider_value(range, width - KNOB / 2., width), 100);
        assert_eq!(slider_value(range, -40., width), 15);
        assert_eq!(slider_value(range, width + 40., width), 100);
        assert_eq!(slider_value(range, width / 2., width), 58);
    }
}
