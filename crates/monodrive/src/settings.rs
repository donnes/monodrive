//! The Settings sheet over the window: a sidebar of pages and grouped rows
//! of macOS controls, after T3 Code's settings modal and Monocode's pages.
//! Every control writes straight to [`Preferences`], so changes apply live.

use crate::agents::{self, Action, Harness, Setup};
use crate::background::{self, Backdrop, Effect, Look, Scope};
use crate::commands::{self, Group};
use crate::frame;
use crate::palettes::{self, PALETTES};
use crate::preferences::{
    self, GRID_ICON_SIZES, Preferences, RowSize, Theme, UI_SCALE_PERCENTS, View, WindowSize,
    WorkspaceStyle,
};
use crate::theme::{
    self, ButtonLook, Elevation, accent, content, focus_ring, icon, icon_button, knob, knob_shadow,
    on_accent, text_button,
};
use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, App, Bounds, ClipboardItem, Context, Div, Entity,
    EventEmitter, FocusHandle, Focusable, FontWeight, KeyBinding, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, PathPromptOptions, Pixels, PromptLevel, Role, ScrollHandle,
    SharedString, Stateful, Subscription, Toggled, Window, actions, anchored, canvas, deferred,
    div, point, prelude::*, px, relative,
};
use monodrive_core::entries::SortBy;
use monodrive_core::mcp::TOOLS;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

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
}

impl Page {
    const ALL: [Page; 5] = [
        Page::General,
        Page::Window,
        Page::Appearance,
        Page::Keyboard,
        Page::Agents,
    ];

    fn label(self) -> &'static str {
        match self {
            Page::General => "General",
            Page::Window => "Window",
            Page::Appearance => "Appearance",
            Page::Keyboard => "Keyboard",
            Page::Agents => "Agents",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Page::General => "sliders",
            Page::Window => "app-window",
            Page::Appearance => "palette",
            Page::Keyboard => "keyboard",
            Page::Agents => "bot",
        }
    }

    fn nav_id(self) -> &'static str {
        match self {
            Page::General => "nav-General",
            Page::Window => "nav-Window",
            Page::Appearance => "nav-Appearance",
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
            Page::Keyboard => Some(|p| p.shortcuts.clear()),
            Page::Agents => Some(|p| p.agents_change_files = true),
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
    handles: RefCell<HashMap<&'static str, FocusHandle>>,
    /// Where each control was last laid out, to scroll the focused one into view.
    probes: Rc<RefCell<HashMap<&'static str, Bounds<Pixels>>>>,
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
    icon: Option<AnyElement>,
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
        Self {
            focus: cx.focus_handle(),
            page: Page::General,
            tracks: Rc::default(),
            dragging: None,
            menu: None,
            menu_active: 0,
            menu_focus: cx.focus_handle(),
            copied: None,
            harness: Harness::ClaudeCode,
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

    fn handle(&self, id: &'static str, cx: &App) -> FocusHandle {
        self.handles
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// Makes `control` a Tab stop with a keyboard focus ring. A disabled
    /// control is skipped. Enter and Space press a focused control's
    /// `on_click`, as GPUI does for any focused element.
    fn focusable(
        &self,
        id: &'static str,
        enabled: bool,
        control: Stateful<Div>,
        cx: &App,
    ) -> Stateful<Div> {
        let handle = self.handle(id, cx).tab_stop(enabled);
        let probes = self.probes.clone();
        control
            .relative()
            .track_focus(&handle)
            .focus_visible(|s| s.shadow(focus_ring()))
            .child(
                canvas(
                    move |bounds, _, _| {
                        probes.borrow_mut().insert(id, bounds);
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
            .find(|(id, handle)| !FIXED_CONTROLS.contains(id) && handle.is_focused(window))
            .map(|(id, _)| *id);
        let Some(bounds) = focused.and_then(|id| self.probes.borrow().get(id).copied()) else {
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

    /// Reads the agents' configs off the UI thread to mark the ones that
    /// already have Monodrive.
    fn check_agents(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let added = cx
                .background_executor()
                .spawn(async { agents::added() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.added = added;
                cx.notify();
            });
        })
        .detach();
    }

    /// Adds Monodrive to `harness`'s config or removes it, off the UI thread.
    fn apply(&mut self, harness: Harness, action: Action, cx: &mut Context<Self>) {
        if self.job.as_ref().is_some_and(|job| job.failed.is_none()) {
            return;
        }
        self.job = Some(Job {
            harness,
            action,
            failed: None,
        });
        cx.notify();
        let exe = agents::exe();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let home = agents::home().ok_or("HOME isn't set.")?;
                    harness.apply(action, &exe, &home)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.job = None;
                        this.over_action = false;
                        // Show the result now; the check confirms it from disk.
                        this.added.retain(|h| *h != harness);
                        if action == Action::Add {
                            this.added.push(harness);
                        }
                        this.check_agents(cx);
                    }
                    Err(error) => {
                        if let Some(job) = &mut this.job {
                            job.failed = Some(error);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The Add button, which shows how the last Add or Remove is going.
    fn action_button(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let harness = self.harness;
        let job = self.job.as_ref().filter(|job| job.harness == harness);
        let added = self.added.contains(&harness);
        let id = "agent-action";
        let keyboard = self.handle(id, cx).is_focused(window) && window.last_input_was_keyboard();
        let frame = |look| {
            theme::button(id, look)
                .min_w(px(112.))
                .on_hover(cx.listener(|this, over: &bool, _, cx| {
                    this.over_action = *over;
                    cx.notify();
                }))
        };
        let (button, next) = match job {
            Some(Job {
                action,
                failed: None,
                ..
            }) => {
                let label = match action {
                    Action::Add => "Adding…",
                    Action::Remove => "Removing…",
                };
                (
                    frame(ButtonLook::Disabled).aria_label(label).child(label),
                    None,
                )
            }
            Some(Job {
                action,
                failed: Some(_),
                ..
            }) => {
                let button = frame(ButtonLook::Caution)
                    .aria_label("Try Again")
                    .child("Try Again");
                (button, Some(*action))
            }
            None if added => {
                let remove = self.over_action || keyboard;
                let label = if remove { "Remove" } else { "Added" };
                let button = frame(ButtonLook::Secondary)
                    .aria_label(format!("Remove from {}", harness.name()))
                    .when(!remove, |d| {
                        d.child(icon("check-hi", 12.).text_color(theme::success()))
                    })
                    .child(label);
                (button, remove.then_some(Action::Remove))
            }
            None => {
                let button = frame(ButtonLook::Primary)
                    .aria_label(format!("Add to {}", harness.name()))
                    .child(format!("Add to {}", harness.name()));
                (button, Some(Action::Add))
            }
        };
        let button = match next {
            Some(action) => {
                button.on_click(cx.listener(move |this, _, _, cx| this.apply(harness, action, cx)))
            }
            None => button,
        };
        let running = job.is_some_and(|job| job.failed.is_none());
        self.focusable(id, !running, button, cx).into_any_element()
    }

    fn pick_harness(&mut self, harness: Harness, cx: &mut Context<Self>) {
        self.harness = harness;
        self.copied = None;
        cx.notify();
    }

    /// Text in a code box with a Copy button that reads Copied once pressed.
    fn code_box(&self, id: &'static str, text: String, cx: &mut Context<Self>) -> AnyElement {
        let copied = self.copied == Some(id);
        let copy = text.clone();
        let label = if copied { "Copied" } else { "Copy" };
        let button = theme::button(id, ButtonLook::Secondary)
            .aria_label(label)
            .when(copied, |d| {
                d.child(icon("check-hi", 12.).text_color(content(0.65)))
            })
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()));
                this.copied = Some(id);
                cx.notify();
            }));
        div()
            .w_full()
            .min_h(px(36.))
            .py(px(6.))
            .pl_3()
            .pr(px(6.))
            .rounded(px(theme::RADIUS_CARD))
            .bg(theme::well())
            .border_1()
            .border_color(content(0.06))
            .flex()
            .items_start()
            .gap_3()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pt(px(4.))
                    .font_family(theme::MONO_FONT)
                    .text_size(px(theme::TEXT_CAPTION))
                    .line_height(px(16.))
                    .child(text),
            )
            .child(self.focusable(id, true, button, cx))
            .into_any_element()
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

    /// Imports the chosen image off the UI thread, then shows it.
    fn choose_background(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let (Some(source), Some(support)) =
                (paths.into_iter().next(), monodrive_core::support_dir())
            else {
                return;
            };
            let imported = cx
                .background_executor()
                .spawn(
                    async move { background::import_image(&source, &support.join("backgrounds")) },
                )
                .await;
            let _ = this.update_in(cx, |this, window, cx| match imported {
                Ok(path) => {
                    preferences::update(cx, |p| p.background = Some(path));
                    // Choose… gives way to Change…; keep the keys there.
                    window.focus(&this.handle("change-background", cx), cx);
                }
                Err(error) => {
                    let answer = window.prompt(
                        PromptLevel::Critical,
                        "Monodrive can't use this image",
                        Some(&error),
                        &["OK"],
                        cx,
                    );
                    cx.background_executor()
                        .spawn(async move {
                            let _ = answer.await;
                        })
                        .detach();
                }
            });
        })
        .detach();
    }

    fn nav(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(NAV_WIDTH))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .gap(px(theme::ROW_GAP))
            .p_2()
            .pt_3()
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
            .child(div().pl_1().text_color(content(0.65)).child("Settings"))
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
                    icon_button("close-settings", "Close", "cancel-01", true)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(Dismissed))),
                    cx,
                ),
            )
    }

    fn general(&self, p: &Preferences, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![group(
            "Files",
            None,
            vec![
                row(
                    "Show hidden files",
                    Some("Files whose names start with a period."),
                    self.focusable(
                        "show-hidden",
                        true,
                        toggle(
                            "show-hidden",
                            "Show hidden files",
                            p.show_hidden,
                            |p, on| p.show_hidden = on,
                        ),
                        cx,
                    ),
                ),
                row(
                    "Sort by",
                    Some("Clicking a column title sorts by it too."),
                    self.focusable(
                        "sort-by",
                        true,
                        segmented(
                            "sort-by",
                            "Sort by",
                            p.sort.by,
                            &[
                                (SortBy::Name, "Name"),
                                (SortBy::Modified, "Date"),
                                (SortBy::Size, "Size"),
                                (SortBy::Kind, "Kind"),
                            ],
                            |p, by| p.sort.by = by,
                        ),
                        cx,
                    ),
                ),
                row(
                    "Order",
                    None,
                    self.focusable(
                        "sort-order",
                        true,
                        segmented(
                            "sort-order",
                            "Order",
                            p.sort.descending,
                            &[(false, "Ascending"), (true, "Descending")],
                            |p, descending| p.sort.descending = descending,
                        ),
                        cx,
                    ),
                ),
                row(
                    "Keep folders on top",
                    Some("Folders come before files in any order."),
                    self.focusable(
                        "folders-first",
                        true,
                        toggle(
                            "folders-first",
                            "Keep folders on top",
                            p.sort.folders_first,
                            |p, on| p.sort.folders_first = on,
                        ),
                        cx,
                    ),
                ),
            ],
        )]
    }

    fn window_page(&self, p: &Preferences, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            group(
                "Window",
                None,
                vec![row(
                    "Default size",
                    Some(
                        "Reasonable is 60% of the screen, up to 1025 × 900, and Almost Full 90%, centered.",
                    ),
                    self.focusable(
                        "window-size",
                        true,
                        segmented(
                            "window-size",
                            "Default size",
                            p.window_size,
                            &[
                                (WindowSize::Reasonable, "Reasonable"),
                                (WindowSize::AlmostFull, "Almost Full"),
                                (WindowSize::LastUsed, "Last Used"),
                            ],
                            |p, size| p.window_size = size,
                        )
                        // Picking the size already chosen restores it after
                        // a manual resize; a new choice resizes once more,
                        // to the same frame.
                        .on_click(|_, window, cx| {
                            frame::apply(cx.global::<Preferences>().window_size, window, cx)
                        }),
                        cx,
                    ),
                )],
            ),
            group(
                "View",
                None,
                vec![
                    row(
                        "View as",
                        Some("⌘1 and ⌘2 switch it too."),
                        self.focusable(
                            "view",
                            true,
                            segmented(
                                "view",
                                "View as",
                                p.view,
                                &[(View::List, "List"), (View::Grid, "Grid")],
                                |p, view| p.view = view,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Show path bar",
                        Some(
                            "The path, selection and free space under the files. ⌥⌘P switches it too.",
                        ),
                        self.focusable(
                            "show-path-bar",
                            true,
                            toggle(
                                "show-path-bar",
                                "Show path bar",
                                p.show_path_bar,
                                |p, on| p.show_path_bar = on,
                            ),
                            cx,
                        ),
                    ),
                ],
            ),
            group(
                "Grid",
                None,
                vec![
                    row(
                        "Icon size",
                        None,
                        self.slider(Slider::GridIcon, p, false, cx),
                    ),
                    row(
                        "Label size",
                        None,
                        self.focusable(
                            "grid-label",
                            true,
                            segmented(
                                "grid-label",
                                "Label size",
                                p.grid_label,
                                &[
                                    (RowSize::Small, "Small"),
                                    (RowSize::Medium, "Medium"),
                                    (RowSize::Large, "Large"),
                                ],
                                |p, size| p.grid_label = size,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Show thumbnails",
                        Some("Images show their pictures instead of an icon."),
                        self.focusable(
                            "thumbnails",
                            true,
                            toggle("thumbnails", "Show thumbnails", p.thumbnails, |p, on| {
                                p.thumbnails = on
                            }),
                            cx,
                        ),
                    ),
                ],
            ),
            group(
                "List",
                None,
                vec![row(
                    "Row size",
                    Some("Icons and text grow with the rows."),
                    self.focusable(
                        "row-size",
                        true,
                        segmented(
                            "row-size",
                            "Row size",
                            p.row_size,
                            &[
                                (RowSize::Small, "Small"),
                                (RowSize::Medium, "Medium"),
                                (RowSize::Large, "Large"),
                            ],
                            |p, size| p.row_size = size,
                        ),
                        cx,
                    ),
                )],
            ),
        ]
    }

    fn agents(&self, p: &Preferences, window: &Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let exe = agents::exe();
        let harness = self.harness;
        let (step, code) = match harness.setup(&exe) {
            Setup::Command(command) => ("Run in a terminal".to_string(), command),
            Setup::Config { file, json } => (format!("Add to {file}"), json),
        };
        let server = div()
            .py(px(12.))
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(32.))
                            .flex_shrink_0()
                            .rounded_full()
                            .bg(content(0.08))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("bot", 16.).text_color(content(1.))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_BODY))
                                    .child("Local MCP server"),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_CAPTION))
                                    .line_height(px(16.))
                                    .text_color(content(0.65))
                                    .child(
                                        "Works with any agent that runs MCP servers over stdio. \
                                         Each agent session starts its own.",
                                    ),
                            ),
                    ),
            )
            .child(self.code_box(
                "copy-server",
                format!("{} mcp", agents::shell_quote(&exe)),
                cx,
            ));
        let harnesses = Harness::ALL;
        let at = harnesses.iter().position(|h| *h == harness).unwrap_or(0);
        let picker = div()
            .id("agent-harness")
            .role(Role::RadioGroup)
            .aria_label("Agent")
            // One Tab stop; the arrows pick, as in a macOS radio group.
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if let Some(step) = arrow(&event.keystroke.key) {
                    let next = at.saturating_add_signed(step).min(harnesses.len() - 1);
                    this.pick_harness(harnesses[next], cx);
                    cx.stop_propagation();
                }
            }))
            .rounded(px(theme::RADIUS_ROW))
            .flex()
            .flex_wrap()
            .gap(px(6.))
            .children(harnesses.into_iter().enumerate().map(|(ix, each)| {
                let selected = each == harness;
                div()
                    .id(("agent-harness", ix))
                    .role(Role::RadioButton)
                    .aria_label(each.name())
                    .aria_selected(selected)
                    .h(px(theme::BUTTON_HEIGHT))
                    .px_3()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(theme::TEXT_SMALL))
                    .border_1()
                    .map(|d| {
                        if selected {
                            d.bg(content(0.14))
                                .border_color(content(0.14))
                                .font_weight(FontWeight::MEDIUM)
                        } else {
                            d.border_color(content(0.06))
                                .text_color(content(0.65))
                                .cursor_pointer()
                                .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                        }
                    })
                    .child(icon(each.icon(), 14.).text_color(content(if selected {
                        1.
                    } else {
                        0.55
                    })))
                    .child(each.name())
                    .when(self.added.contains(&each), |d| d.child(added_dot()))
                    .on_click(cx.listener(move |this, _, _, cx| this.pick_harness(each, cx)))
            }));
        let failed = self
            .job
            .as_ref()
            .filter(|job| job.harness == harness)
            .and_then(|job| job.failed.clone());
        let about = match &failed {
            Some(error) => div().text_color(theme::danger()).child(error.clone()),
            None if self.added.contains(&harness) => {
                div().text_color(content(0.65)).child(harness.then())
            }
            None => div().text_color(content(0.65)).child(harness.adds()),
        };
        let by_hand = self.by_hand;
        let disclosure = div()
            .id("agent-by-hand")
            .role(Role::Button)
            .aria_label("Do it yourself")
            .aria_expanded(by_hand)
            .self_start()
            .rounded_full()
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(theme::TEXT_CAPTION))
            .text_color(content(0.65))
            .cursor_pointer()
            .hover(|s| s.text_color(content(1.)))
            .child(
                icon(
                    if by_hand {
                        "chevron-down-hi"
                    } else {
                        "chevron-right"
                    },
                    12.,
                )
                .text_color(content(0.55)),
            )
            .child("Do it yourself")
            .on_click(cx.listener(|this, _, _, cx| {
                this.by_hand = !this.by_hand;
                cx.notify();
            }));
        let setup = div()
            .py(px(12.))
            .flex()
            .flex_col()
            .gap_4()
            .child(self.focusable("agent-harness", true, picker, cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_4()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(div().text_size(px(theme::TEXT_BODY)).child(harness.name()))
                            .child(
                                about
                                    .text_size(px(theme::TEXT_CAPTION))
                                    .line_height(px(16.)),
                            ),
                    )
                    .child(self.action_button(window, cx)),
            )
            .child(self.focusable("agent-by-hand", true, disclosure, cx))
            .when(by_hand, |d| {
                d.child(step_row(
                    1,
                    step,
                    Some(self.code_box("copy-setup", code, cx)),
                ))
                .child(step_row(2, harness.then().to_string(), None))
            });
        let tools = TOOLS
            .iter()
            .map(|tool| {
                div()
                    .py(px(8.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .font_family(theme::MONO_FONT)
                            .text_size(px(theme::TEXT_SMALL))
                            .child(tool.name),
                    )
                    .child(
                        div()
                            .text_size(px(theme::TEXT_CAPTION))
                            .line_height(px(16.))
                            .text_color(content(0.65))
                            .child(tool.description),
                    )
                    .into_any_element()
            })
            .collect();
        vec![
            group(
                "MCP",
                Some(
                    "Agents run Monodrive as an MCP server and search the same index as this window.",
                ),
                vec![server.into_any_element(), setup.into_any_element()],
            ),
            group(
                "Files",
                None,
                vec![row(
                    "Let agents change files",
                    Some(
                        "Trash, put back, rename, create, duplicate, undo and redo. Off, agents can only read.",
                    ),
                    self.focusable(
                        "agents-change-files",
                        true,
                        toggle(
                            "agents-change-files",
                            "Let agents change files",
                            p.agents_change_files,
                            |p, on| p.agents_change_files = on,
                        ),
                        cx,
                    ),
                )],
            ),
            group(
                "Tools",
                Some("What agents can call. The ones that change files need the setting above."),
                tools,
            ),
        ]
    }

    /// Every command and its shortcut, grouped as in the menu bar.
    fn keyboard(&self, p: &Preferences, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let all = commands::all();
        let mut page = Vec::new();
        if let Some(error) = &self.recording_error {
            page.push(
                div()
                    .px_1()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(theme::danger())
                    .child(error.clone())
                    .into_any_element(),
            );
        }
        for group_of in Group::ALL {
            let rows = all
                .iter()
                .filter(|command| command.group == group_of)
                .map(|command| {
                    let id = command.id();
                    let context = command.context;
                    let recording = self.recording == Some(id);
                    let changed = p.shortcuts.contains_key(id);
                    let keys = command.shortcuts(&p.shortcuts);
                    let shown = if recording {
                        "Type a shortcut…".to_string()
                    } else if keys.is_empty() {
                        "None".to_string()
                    } else {
                        keys.iter()
                            .map(|keys| commands::label(keys))
                            .collect::<Vec<_>>()
                            .join("  ")
                    };
                    let button = theme::button(id, ButtonLook::Secondary)
                        .aria_label(format!("{}: {shown}", command.title))
                        .min_w(px(96.))
                        .border_1()
                        .border_color(if recording {
                            accent()
                        } else {
                            gpui::transparent_black()
                        })
                        .when(recording, |d| d.bg(accent().opacity(0.2)))
                        .when(keys.is_empty() && !recording, |d| {
                            d.text_color(content(0.65))
                        })
                        .child(shown)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_recording(id, context, window, cx)
                        }));
                    row(
                        command.title,
                        None,
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(changed, |d| {
                                d.child(
                                    theme::row_icon_button(
                                        SharedString::from(format!("reset-{id}")),
                                        "Use the default",
                                        "rotate-ccw",
                                        content(0.55),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| this.reset_shortcut(id, context, cx),
                                    )),
                                )
                            })
                            .child(self.focusable(id, true, button, cx)),
                    )
                })
                .collect();
            page.push(group(group_of.label(), None, rows));
        }
        page
    }

    /// Records the next key press as `id`'s shortcut, ahead of everything
    /// that would otherwise run it: ⌘, records rather than opening Settings.
    fn start_recording(
        &mut self,
        id: &'static str,
        context: Option<&'static str>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.recording = Some(id);
        self.recording_error = None;
        let this = cx.weak_entity();
        self.recorder = Some(cx.intercept_keystrokes(move |event, _, cx| {
            let keystroke = event.keystroke.clone();
            let m = keystroke.modifiers;
            // Tab gives up and moves on, so recording never traps the keyboard.
            let tab = keystroke.key == "tab" && !(m.platform || m.control || m.alt);
            if !tab {
                cx.stop_propagation();
            }
            let _ = this.update(cx, |this, cx| {
                if tab {
                    this.recording_error = None;
                    this.stop_recording(cx);
                } else {
                    this.record(id, context, &keystroke, cx);
                }
            });
        }));
        cx.notify();
    }

    /// Puts `id` back on its default keys, unless another command has taken
    /// one of them since.
    fn reset_shortcut(
        &mut self,
        id: &'static str,
        context: Option<&'static str>,
        cx: &mut Context<Self>,
    ) {
        let all = commands::all();
        let Some(command) = all.iter().find(|command| command.id() == id) else {
            return;
        };
        let taken = command.keys.iter().find_map(|keys| {
            let stroke = gpui::Keystroke::parse(keys).ok()?;
            let other = commands::conflict(&stroke, id, context, cx)?;
            Some(format!(
                "{} is already used by {other}.",
                commands::label(keys)
            ))
        });
        match taken {
            Some(error) => self.recording_error = Some(error),
            None => {
                self.recording_error = None;
                preferences::update(cx, |p| {
                    p.shortcuts.remove(id);
                });
            }
        }
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.recorder = None;
        cx.notify();
    }

    fn record(
        &mut self,
        id: &'static str,
        context: Option<&'static str>,
        keystroke: &gpui::Keystroke,
        cx: &mut Context<Self>,
    ) {
        let m = keystroke.modifiers;
        let key = keystroke.key.as_str();
        match key {
            "escape" if !m.modified() => {
                self.recording_error = None;
                self.stop_recording(cx);
                return;
            }
            "backspace" | "delete" if !m.modified() => {
                preferences::update(cx, |p| {
                    p.shortcuts.insert(id.to_string(), None);
                });
                self.recording_error = None;
                self.stop_recording(cx);
                return;
            }
            _ => {}
        }
        let function_key = key.len() > 1 && key.starts_with('f') && key[1..].parse::<u8>().is_ok();
        if !(m.platform || m.control || m.alt || function_key) {
            self.recording_error = Some("Use ⌘, ⌃ or ⌥ with a key, or a function key.".into());
            cx.notify();
            return;
        }
        let keys = keystroke.unparse();
        if let Some(other) = commands::conflict(keystroke, id, context, cx) {
            self.recording_error = Some(format!(
                "{} is already used by {other}.",
                commands::label(&keys)
            ));
            cx.notify();
            return;
        }
        preferences::update(cx, |p| {
            p.shortcuts.insert(id.to_string(), Some(keys));
        });
        self.recording_error = None;
        self.stop_recording(cx);
    }

    fn appearance(
        &self,
        p: &Preferences,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let light = theme::is_light();
        let swatch_for = |id: &str| {
            let palette = palettes::palette(id, light);
            swatch(palette.surface, palette.accent).into_any_element()
        };
        let presets = PALETTES
            .iter()
            .map(|&(id, label, _, _)| MenuItem {
                label: label.into(),
                icon: Some(swatch_for(id)),
                selected: id == p.palette,
                choose: Rc::new(move |cx| preferences::update(cx, |p| p.apply_palette(id))),
            })
            .collect();
        let scales = UI_SCALE_PERCENTS
            .into_iter()
            .map(|percent| MenuItem {
                label: format!("{percent}%").into(),
                icon: None,
                selected: percent == p.ui_scale,
                choose: Rc::new(move |cx| preferences::update(cx, |p| p.ui_scale = percent)),
            })
            .collect();
        vec![
            group(
                "Theme",
                None,
                vec![
                    row(
                        "Appearance",
                        Some("System follows macOS."),
                        self.focusable(
                            "theme",
                            true,
                            segmented(
                                "theme",
                                "Appearance",
                                p.theme,
                                &[
                                    (Theme::System, "System"),
                                    (Theme::Dark, "Dark"),
                                    (Theme::Light, "Light"),
                                ],
                                |p, theme| p.theme = theme,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Color preset",
                        Some("Sets the ink, the accent and the window tint below."),
                        self.popup("palette", "Color preset", 150., presets, window, cx),
                    ),
                    row(
                        "Accent color",
                        Some("Selection, switches and sliders."),
                        self.focusable("accent", true, accent_picker(p, light), cx),
                    ),
                ],
            ),
            group(
                "Color",
                None,
                vec![
                    row(
                        "Hue",
                        Some("Tints the window."),
                        self.slider(Slider::Hue, p, false, cx),
                    ),
                    row(
                        "Saturation",
                        Some("How strongly the hue shows. Zero keeps it neutral."),
                        self.slider(Slider::Saturation, p, false, cx),
                    ),
                    row(
                        "Dark-mode lightness",
                        Some(if light {
                            "Applies in dark mode only."
                        } else {
                            "Lower is darker; zero is black."
                        }),
                        self.slider(Slider::DarkLightness, p, light, cx),
                    ),
                ],
            ),
            group(
                "Translucency",
                Some(if light {
                    "Translucency is available in dark mode."
                } else {
                    "How much of the desktop shows through Monodrive."
                }),
                vec![
                    row(
                        "Window opacity",
                        Some("The sidebar and title bar."),
                        self.slider(Slider::SidebarOpacity, p, light, cx),
                    ),
                    row(
                        "Main pane glass",
                        Some("Lets the file list show the desktop too."),
                        div().when(light, |d| d.opacity(0.4)).child(self.focusable(
                            "main-pane-glass",
                            !light,
                            toggle(
                                "main-pane-glass",
                                "Main pane glass",
                                p.main_pane_glass,
                                |p, on| p.main_pane_glass = on,
                            ),
                            cx,
                        )),
                    ),
                ],
            ),
            self.background_group(p, cx),
            group(
                "Layout",
                None,
                vec![
                    row(
                        "Workspace style",
                        Some("A rounded card or a full-height boxy workspace."),
                        self.focusable(
                            "workspace-style",
                            true,
                            segmented(
                                "workspace-style",
                                "Workspace style",
                                p.workspace_style,
                                &[
                                    (WorkspaceStyle::Rounded, "Rounded"),
                                    (WorkspaceStyle::Boxy, "Boxy"),
                                ],
                                |p, style| p.workspace_style = style,
                            ),
                            cx,
                        ),
                    ),
                    row(
                        "Interface scale",
                        Some("Zooms the whole window."),
                        self.popup("ui-scale", "Interface scale", 90., scales, window, cx),
                    ),
                ],
            ),
        ]
    }

    fn background_group(&self, p: &Preferences, cx: &mut Context<Self>) -> AnyElement {
        const TITLE: &str = "Background";
        const DESCRIPTION: &str = "An image behind the file list. It stays on this Mac.";
        let Some(path) = p.background.clone() else {
            self.preview.update(cx, |preview, cx| preview.set(None, cx));
            return group(
                TITLE,
                Some(DESCRIPTION),
                vec![row(
                    "Image",
                    None,
                    self.focusable(
                        "choose-background",
                        true,
                        text_button("choose-background", "Choose…", ButtonLook::Secondary)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_background(window, cx)
                            })),
                        cx,
                    ),
                )],
            );
        };
        let empty = p.background_empty;
        self.preview.update(cx, |preview, cx| {
            preview.set(
                Some(Look {
                    path,
                    effect: p.background_effect,
                    opacity: empty as f32 / 100.,
                    empty: true,
                    preview: true,
                    radius: theme::RADIUS_CARD - 1.,
                }),
                cx,
            )
        });
        let preview = div()
            .py(px(12.))
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(
                div()
                    .relative()
                    .h(px(132.))
                    .w_full()
                    .rounded(px(theme::RADIUS_CARD))
                    .border_1()
                    .border_color(content(0.12))
                    .child(self.preview.clone())
                    .child(
                        div()
                            .absolute()
                            .bottom_2()
                            .left_3()
                            .text_size(px(theme::TEXT_CAPTION))
                            .text_color(content(0.65))
                            .child(format!("Empty folder preview at {empty}%")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        self.focusable(
                            "change-background",
                            true,
                            text_button("change-background", "Change…", ButtonLook::Secondary)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_background(window, cx)
                                })),
                            cx,
                        ),
                    )
                    .child(self.focusable(
                        "remove-background",
                        true,
                        text_button("remove-background", "Remove", ButtonLook::Secondary).on_click(
                            cx.listener(|this, _, window, cx| {
                                preferences::update(cx, |p| p.background = None);
                                // Remove goes away with the image; Choose… takes its place.
                                window.focus(&this.handle("choose-background", cx), cx);
                            }),
                        ),
                        cx,
                    )),
            )
            .into_any_element();
        let effects: Vec<_> = Effect::ALL.iter().map(|&e| (e, e.label())).collect();
        group(
            TITLE,
            Some(DESCRIPTION),
            vec![
                preview,
                row(
                    "Background effect",
                    Some(p.background_effect.description()),
                    self.focusable(
                        "background-effect",
                        true,
                        segmented(
                            "background-effect",
                            "Background effect",
                            p.background_effect,
                            &effects,
                            |p, effect| p.background_effect = effect,
                        ),
                        cx,
                    ),
                ),
                row(
                    "Show in",
                    Some("Empty folders only, or every folder."),
                    self.focusable(
                        "background-scope",
                        true,
                        segmented(
                            "background-scope",
                            "Show in",
                            p.background_scope,
                            &[(Scope::Empty, "Empty only"), (Scope::All, "All folders")],
                            |p, scope| p.background_scope = scope,
                        ),
                        cx,
                    ),
                ),
                row(
                    "Empty folder visibility",
                    Some("Background strength in a folder with nothing in it."),
                    self.slider(Slider::BackgroundEmpty, p, false, cx),
                ),
                row(
                    "Folder visibility",
                    Some("Background strength behind a folder's items."),
                    self.slider(
                        Slider::BackgroundFolder,
                        p,
                        p.background_scope == Scope::Empty,
                        cx,
                    ),
                ),
            ],
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
        let mut items = items;
        let trigger_icon = current_icon
            .then(|| items.iter_mut().find(|item| item.selected))
            .flatten()
            .and_then(|item| item.icon.take());
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
                            .pl(px(6.))
                            .pr_3()
                            .gap(px(6.))
                            .child(div().w(px(14.)).flex_shrink_0().when(selected, |d| {
                                d.child(icon("check-hi", 12.).text_color(if ix == active {
                                    on_accent()
                                } else {
                                    content(1.)
                                }))
                            }))
                            .children(item_icon)
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
                                        .p_1()
                                        .child(
                                            div()
                                                .id("menu-items")
                                                .relative()
                                                .max_h(px(348.))
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
            )
            .with_animation(
                "settings-enter",
                Animation::new(Duration::from_millis(200)).with_easing(ease_out),
                |card, t| card.opacity(t).top(px(6. * (1. - t))),
            );
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
            .with_animation(
                "settings-scrim",
                Animation::new(Duration::from_millis(200)).with_easing(ease_out),
                |scrim, t| scrim.bg(theme::scrim(t)),
            )
    }
}

fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(3)
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
                .px_1()
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
                        .mx(px(14.))
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
                .child(div().text_size(px(theme::TEXT_BODY)).child(label))
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
