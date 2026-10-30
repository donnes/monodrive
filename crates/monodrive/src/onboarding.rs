//! First launch, in a window of its own: what Monodrive is and the keys to
//! know, then one checklist of Full Disk Access and coding agents. It comes
//! before the main windows and before the first crawl, which would otherwise
//! run without the access and again after it. Show Welcome and the sidebar's
//! notice open it later.

use crate::agents::{self, Action, Harness};
use crate::assets::LOGO;
use crate::browser::{self, QuickLook, SharedIndex};
use crate::commands::{self, OpenPalette};
use crate::preferences::{self, Preferences};
use crate::theme::{self, ButtonLook, content, icon, text_button};
use crate::traffic_lights;
use async_channel::{Receiver, Sender};
use gpui::{
    Action as _, AnyElement, App, ClickEvent, Context, Div, ExternalDragPayload, FileDragPaths,
    FocusHandle, Focusable, FontWeight, Global, ImageSource, Img, KeyBinding, Resource, Role,
    Stateful, TitlebarOptions, Window, WindowAppearance, WindowBackgroundAppearance, WindowBounds,
    WindowHandle, WindowId, WindowOptions, actions, div, img, point, prelude::*, px, size,
};
use monodrive_core::index::{self, Index};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

/// Raise it to show onboarding once more to everyone who finished this one.
pub const VERSION: u32 = 1;

const WIDTH: f32 = 640.;
const HEIGHT: f32 = 520.;
/// The drag image of the app, as a file drag's is.
const DRAG_ICON: f32 = 32.;
/// The pane in System Settings that lists what has Full Disk Access.
const PRIVACY_PANE: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";
/// Why a search has no answer yet, for the windows ⌘N opens during onboarding.
const WAITING: &str = "Search starts when you finish setting up Monodrive.";

actions!(
    onboarding,
    [Continue, GoBack, CloseWelcome, FocusNext, FocusPrevious]
);

pub fn bind(cx: &mut App) {
    let context = Some("Onboarding");
    cx.bind_keys([
        KeyBinding::new("enter", Continue, context),
        KeyBinding::new("escape", GoBack, context),
        KeyBinding::new("cmd-w", CloseWelcome, context),
        KeyBinding::new("tab", FocusNext, context),
        KeyBinding::new("shift-tab", FocusPrevious, context),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Welcome,
    Checklist,
}

/// What onboarding asks of the Mac, behind a trait so tests never read or
/// change real permissions, start the real index or touch real agent configs.
pub trait Setup: Send + Sync {
    /// Whether this process has Full Disk Access. Call it off the UI thread.
    fn full_disk_access(&self) -> bool;
    /// Whether the index running in this process started without the access,
    /// so it skips those folders until the next launch, whatever macOS says
    /// now. Call it off the UI thread.
    fn index_lacks_access(&self) -> bool;
    /// Whether an index is saved, so starting one loads it instead of crawling.
    fn saved_index(&self) -> bool;
    /// Starts the index. Call it off the UI thread.
    fn start_index(&self) -> Result<Index, String>;
    fn open_privacy_settings(&self, cx: &App);
    /// What macOS grants the access to: the app, or a bare binary's path.
    fn app(&self) -> PathBuf;
    /// Quits and opens the same app again.
    fn relaunch(&self, cx: &mut App);
    /// The agents to offer, and those that have Monodrive. Call it off the UI
    /// thread.
    fn agents(&self) -> (Vec<Harness>, Vec<Harness>);
    /// Blocks on the disk and the agent's CLI, so call it off the UI thread.
    fn apply_agent(&self, harness: Harness, action: Action) -> Result<(), String>;
}

/// The real thing.
pub struct System {
    index: Arc<OnceLock<Result<Index, String>>>,
}

impl System {
    pub fn new(index: Arc<OnceLock<Result<Index, String>>>) -> Self {
        Self { index }
    }
}

impl Setup for System {
    fn full_disk_access(&self) -> bool {
        index::has_full_disk_access()
    }

    fn index_lacks_access(&self) -> bool {
        self.index
            .get()
            .and_then(|index| index.as_ref().ok())
            .is_some_and(|index| index.status().full_disk_access == Some(false))
    }

    fn saved_index(&self) -> bool {
        index::saved()
    }

    fn start_index(&self) -> Result<Index, String> {
        #[expect(
            clippy::disallowed_methods,
            reason = "the app starts the index at launch, once onboarding lets it"
        )]
        Index::start()
    }

    fn open_privacy_settings(&self, cx: &App) {
        cx.open_url(PRIVACY_PANE);
    }

    fn app(&self) -> PathBuf {
        std::env::current_exe().map_or_else(|_| PathBuf::from("Monodrive"), |exe| app_of(&exe))
    }

    fn relaunch(&self, cx: &mut App) {
        cx.restart();
    }

    fn agents(&self) -> (Vec<Harness>, Vec<Harness>) {
        (agents::detected(), agents::added())
    }

    fn apply_agent(&self, harness: Harness, action: Action) -> Result<(), String> {
        let home = agents::home().ok_or("HOME isn't set.")?;
        harness.apply(action, &agents::exe(), &home)
    }
}

/// The bundle `exe` runs from, or `exe` itself if it's a bare binary.
fn app_of(exe: &Path) -> PathBuf {
    exe.ancestors()
        .skip(1)
        .find(|dir| dir.extension().is_some_and(|ext| ext == "app"))
        .unwrap_or(exe)
        .to_path_buf()
}

/// Whether the index may start yet, and what starts it. `launch` sets it. It's
/// held only while onboarding is up on a Mac with no saved index and no access:
/// a crawl now would run again for what it skipped once the access comes.
#[derive(Clone)]
pub struct IndexGate {
    setup: Arc<dyn Setup>,
    /// Closed once the index may start.
    opened: Receiver<()>,
    _sender: Sender<()>,
}

impl Global for IndexGate {}

impl IndexGate {
    pub(crate) fn new(setup: Arc<dyn Setup>, held: bool) -> Self {
        let (sender, opened) = async_channel::bounded(1);
        if !held {
            opened.close();
        }
        Self {
            setup,
            opened,
            _sender: sender,
        }
    }

    pub fn held(&self) -> bool {
        !self.opened.is_closed()
    }

    fn open(&self) {
        self.opened.close();
    }

    /// Returns once the index may start.
    pub async fn wait(&self) {
        let _ = self.opened.recv().await;
    }

    /// Starts the index unless it has started. Call it off the UI thread.
    pub fn start<'a>(
        &self,
        index: &'a OnceLock<Result<Index, String>>,
    ) -> &'a Result<Index, String> {
        index.get_or_init(|| self.setup.start_index())
    }

    /// The index for a search, started if it may be, or why there's none yet.
    /// Call it off the UI thread.
    pub fn for_search<'a>(
        &self,
        index: &'a OnceLock<Result<Index, String>>,
    ) -> Result<&'a Index, String> {
        if self.held() && index.get().is_none() {
            return Err(WAITING.into());
        }
        self.start(index).as_ref().map_err(Clone::clone)
    }
}

type Then = Box<dyn FnOnce(&mut App)>;

/// Opens a main window when onboarding leaves the app with none.
struct Reopen(Rc<dyn Fn(&mut App)>);
impl Global for Reopen {}

/// The onboarding window while it's up, and what opens when it goes.
struct Flow {
    window: WindowHandle<Onboarding>,
    then: Option<Then>,
}

impl Global for Flow {}

/// Starts the app: onboarding if it's due, which `then` follows, or `then` at
/// once. A launch on a `folder` never onboards, and nor does a finished Mac.
pub fn launch(
    explicit_folder: bool,
    setup: Arc<dyn Setup>,
    then: impl FnOnce(&mut App) + 'static,
    reopen: impl Fn(&mut App) + 'static,
    cx: &mut App,
) {
    cx.set_global(Reopen(Rc::new(reopen)));
    let due = !explicit_folder && cx.global::<Preferences>().onboarding_version < VERSION;
    if !due {
        cx.set_global(IndexGate::new(setup, false));
        return then(cx);
    }
    // Held until the disk has been asked, off the UI thread.
    let gate = IndexGate::new(setup.clone(), true);
    cx.set_global(gate.clone());
    let then: Then = Box::new(then);
    cx.spawn(async move |cx| {
        let (granted, held) = cx
            .background_executor()
            .spawn(async move {
                let granted = setup.full_disk_access();
                (granted, !granted && !setup.saved_index())
            })
            .await;
        if !held {
            gate.open();
        }
        // With the access there's nothing to explain, so it starts where
        // there's something to decide: an existing install, or a reopen after
        // the grant.
        let step = if granted {
            Step::Checklist
        } else {
            Step::Welcome
        };
        cx.update(|cx| {
            open(step, Some(then), cx);
            cx.activate(true);
        });
    })
    .detach();
}

/// Opens onboarding at `step`, or takes the open one there.
pub fn show(step: Step, cx: &mut App) {
    if let Some(window) = cx.try_global::<Flow>().map(|flow| flow.window) {
        // Later, as a command can come from inside that same window.
        cx.defer(move |cx| {
            let _ = window.update(cx, |view, window, cx| {
                view.go(step, cx);
                window.activate_window();
            });
        });
        return;
    }
    open(step, None, cx);
}

/// Opens what onboarding is holding back, saying whether it held anything.
pub fn open_windows_now(cx: &mut App) -> bool {
    let then = cx
        .try_global::<Flow>()
        .is_some()
        .then(|| cx.global_mut::<Flow>().then.take())
        .flatten();
    match then {
        Some(then) => {
            then(cx);
            true
        }
        None => false,
    }
}

/// A window closed. If it's onboarding's, however it went, what it held back
/// goes ahead, and if that was already opened and closed, a main window does,
/// before the app sees it has no windows left and quits.
pub fn closed(window: WindowId, cx: &mut App) {
    if cx
        .try_global::<Flow>()
        .is_none_or(|flow| flow.window.window_id() != window)
    {
        return;
    }
    let then = cx.remove_global::<Flow>().then;
    open_gate(cx);
    if let Some(then) = then {
        then(cx);
    } else if !browser::any_window(cx)
        && let Some(reopen) = cx.try_global::<Reopen>().map(|reopen| reopen.0.clone())
    {
        reopen(cx);
    }
}

fn open_gate(cx: &App) {
    if let Some(gate) = cx.try_global::<IndexGate>() {
        gate.open();
    }
}

/// The access is there, so the index can get going without waiting for the
/// rest of the checklist.
fn start_index(cx: &mut App) {
    open_gate(cx);
    let (Some(gate), Some(index)) = (
        cx.try_global::<IndexGate>().cloned(),
        cx.try_global::<SharedIndex>()
            .map(|shared| shared.0.clone()),
    ) else {
        return;
    };
    cx.background_executor()
        .spawn(async move {
            let _ = gate.start(&index);
        })
        .detach();
}

fn open(step: Step, then: Option<Then>, cx: &mut App) {
    let Some(setup) = cx.try_global::<IndexGate>().map(|gate| gate.setup.clone()) else {
        return;
    };
    let bounds = gpui::Bounds::centered(None, size(px(WIDTH), px(HEIGHT)), cx);
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            is_resizable: false,
            window_background: WindowBackgroundAppearance::Blurred,
            app_owns_titlebar_drag: true,
            titlebar: Some(TitlebarOptions {
                title: Some("Welcome to Monodrive".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(
                    px(12.),
                    px((theme::TITLE_BAR_HEIGHT - 14.) / 2.),
                )),
            }),
            ..Default::default()
        },
        |window, cx| {
            let view = cx.new(|cx| Onboarding::new(step, setup, cx));
            window.focus(&view.focus_handle(cx), cx);
            traffic_lights::pin(window, theme::TITLE_BAR_HEIGHT);
            view.update(cx, |_, cx| {
                // Back from System Settings, the grant may be there.
                cx.observe_window_activation(window, |this, window, cx| {
                    traffic_lights::pin(window, theme::TITLE_BAR_HEIGHT);
                    if window.is_window_active() {
                        this.check_access(cx);
                    }
                })
                .detach();
                cx.observe_window_appearance(window, |_, _, cx| cx.notify())
                    .detach();
                cx.observe_global::<Preferences>(|_, cx| cx.notify())
                    .detach();
            });
            window.activate_window();
            window.on_next_frame(|_, cx| commands::menus_ready(cx));
            view
        },
    );
    if let Ok(window) = opened {
        cx.set_global(Flow { window, then });
    }
}

/// What this process can do with Full Disk Access.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Access {
    granted: bool,
    index_lacks: bool,
}

/// The app, being dragged out to System Settings' list.
struct AppDrag {
    path: PathBuf,
    folder: bool,
}

struct AppDragImage;

impl Render for AppDragImage {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        logo(DRAG_ICON)
    }
}

pub struct Onboarding {
    step: Step,
    access: Access,
    /// System Settings was opened from here, so a grant may be on its way.
    asked: bool,
    setup: Arc<dyn Setup>,
    app: PathBuf,
    /// `None` until the configs have been read.
    agents: Option<Vec<Harness>>,
    added: Vec<Harness>,
    busy: Option<Harness>,
    failed: Option<(Harness, String)>,
    focus: FocusHandle,
    /// The Tab stops, kept across frames.
    stops: RefCell<HashMap<&'static str, FocusHandle>>,
    glass: Option<bool>,
}

impl Focusable for Onboarding {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Onboarding {
    pub fn new(step: Step, setup: Arc<dyn Setup>, cx: &mut Context<Self>) -> Self {
        let app = setup.app();
        let mut this = Self {
            step: Step::Welcome,
            access: Access::default(),
            asked: false,
            setup,
            app,
            agents: None,
            added: Vec::new(),
            busy: None,
            failed: None,
            focus: cx.focus_handle(),
            stops: RefCell::default(),
            glass: None,
        };
        this.check_access(cx);
        this.go(step, cx);
        this
    }

    fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        if step == Step::Checklist {
            self.read_agents(cx);
        }
        cx.notify();
    }

    /// Asks again whether the access is there. It never fails or prompts, and
    /// nothing repaints unless the answer changed.
    pub fn check_access(&mut self, cx: &mut Context<Self>) {
        let setup = self.setup.clone();
        cx.spawn(async move |this, cx| {
            let access = cx
                .background_executor()
                .spawn(async move {
                    Access {
                        granted: setup.full_disk_access(),
                        index_lacks: setup.index_lacks_access(),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.access != access {
                    if access.granted {
                        start_index(cx);
                    }
                    this.access = access;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn read_agents(&mut self, cx: &mut Context<Self>) {
        let setup = self.setup.clone();
        cx.spawn(async move |this, cx| {
            let (detected, added) = cx
                .background_executor()
                .spawn(async move { setup.agents() })
                .await;
            let _ = this.update(cx, |this, cx| {
                let also = added.iter().copied().filter(|h| !detected.contains(h));
                let shown = detected.iter().copied().chain(also).collect();
                this.agents = Some(shown);
                this.added = added;
                cx.notify();
            });
        })
        .detach();
    }

    /// What ↩ does: the button in the accent color.
    pub fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.step {
            Step::Welcome => self.go(Step::Checklist, cx),
            Step::Checklist if self.access.granted => self.finish(window, cx),
            Step::Checklist => self.open_settings(cx),
        }
    }

    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.asked = true;
        self.setup.open_privacy_settings(cx);
        cx.notify();
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        preferences::update(cx, |p| p.onboarding_version = VERSION);
        window.remove_window();
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if self.step == Step::Checklist {
            self.go(Step::Welcome, cx);
        }
    }

    /// Adds Monodrive to `harness` or takes it out, off the UI thread. One at
    /// a time: a CLI can take seconds.
    fn toggle_agent(&mut self, harness: Harness, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        let action = if self.added.contains(&harness) {
            Action::Remove
        } else {
            Action::Add
        };
        self.busy = Some(harness);
        self.failed = None;
        cx.notify();
        let setup = self.setup.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { setup.apply_agent(harness, action) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(()) => {
                        this.added.retain(|h| *h != harness);
                        if action == Action::Add {
                            this.added.push(harness);
                        }
                    }
                    Err(why) => this.failed = Some((harness, why)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Runs `act` for a press, and hands the keys back to the window so ↩
    /// goes on meaning the primary button, whatever the press took away.
    fn press(
        &self,
        act: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
        cx.listener(move |this, _, window, cx| {
            window.focus(&this.focus, cx);
            act(this, window, cx);
        })
    }

    /// Makes `control` a Tab stop with a focus ring. Space presses it, and so
    /// does ↩, as GPUI does for any focused element.
    fn stop(&self, id: &'static str, control: Stateful<Div>, cx: &App) -> Stateful<Div> {
        let handle = self
            .stops
            .borrow_mut()
            .entry(id)
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone();
        control
            .track_focus(&handle)
            .focus_visible(|s| s.shadow(theme::focus_ring()))
    }

    fn button(
        &self,
        id: &'static str,
        label: &'static str,
        look: ButtonLook,
        act: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let pressed = self.press(act, cx);
        self.stop(id, text_button(id, label, look), cx)
            .debug_selector(move || id.into())
            .on_click(pressed)
    }

    fn welcome(&self, cx: &App) -> Div {
        let first = |id: &str| {
            commands::shortcuts_of(id, cx)
                .first()
                .map(|keys| commands::label(keys))
        };
        // ⌘Y comes first for the menu bar; Space is the one to know.
        let quick_look = commands::shortcuts_of(QuickLook.name(), cx)
            .last()
            .map(|keys| commands::label(keys));
        let keys = [
            (first(OpenPalette.name()), "Command palette"),
            (quick_look, "Quick Look"),
            (first(browser::NewTab.name()), "New tab"),
        ];
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(logo(96.).mb_2())
            .child(title("Welcome to Monodrive"))
            .child(
                div()
                    .max_w(px(420.))
                    .text_center()
                    .text_size(px(theme::TEXT_BODY))
                    .text_color(content(0.65))
                    .child(
                        "A fast file manager that searches your whole disk as you type, for you and your coding agents.",
                    ),
            )
            .child(
                div().mt_4().flex().items_center().gap_4().children(
                    keys.into_iter()
                        .filter_map(|(keys, what)| Some((keys?, what)))
                        .map(|(keys, what)| {
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    div()
                                        .h(px(theme::BUTTON_HEIGHT))
                                        .px_2()
                                        .flex()
                                        .items_center()
                                        .rounded_full()
                                        .bg(content(0.08))
                                        .text_size(px(theme::TEXT_SMALL))
                                        .child(keys),
                                )
                                .child(note(what))
                        }),
                ),
            )
    }

    fn access_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let granted = self.access.granted;
        // The index keeps skipping what it skipped when it started; macOS may
        // also hold a new grant back from a process that's already running.
        let reopens = if granted {
            self.access.index_lacks
        } else {
            self.asked
        };
        let subtitle = if granted && reopens {
            "The index started without it. Reopen to include those folders."
        } else if !granted && self.bare() {
            "A bare binary: macOS asks about the app that started it. Use Monodrive.app."
        } else if !granted && self.asked {
            "Turn on Monodrive in the list, or drag this row into it. Then come back."
        } else {
            "Search Desktop, Documents and Mail. Nothing leaves this Mac."
        };
        let trailing = div()
            .flex()
            .items_center()
            .gap_2()
            .when(granted, |d| {
                d.child(done("Granted").debug_selector(|| "access-granted".into()))
            })
            .when(reopens, |d| {
                d.child(
                    self.button(
                        "access-reopen",
                        "Reopen",
                        ButtonLook::Ghost,
                        |this, _, cx| this.setup.relaunch(cx),
                        cx,
                    )
                    .aria_label("Reopen Monodrive"),
                )
            })
            .when(!granted, |d| {
                d.child(self.button(
                    "access-grant",
                    "Grant",
                    ButtonLook::Primary,
                    |this, _, cx| this.open_settings(cx),
                    cx,
                ))
            })
            .into_any_element();
        let path = self.app.clone();
        let folder = !self.bare();
        row(
            logo(theme::SIDEBAR_ROW_HEIGHT - 6.).into_any_element(),
            "Full Disk Access",
            div().text_color(content(0.65)).child(subtitle),
            trailing,
        )
        .id("access-row")
        .when(!granted, move |d| {
            d.on_drag(AppDrag { path, folder }, |_, _, _, cx| {
                cx.new(|_| AppDragImage)
            })
            .external_drag_payload(|drag: &AppDrag, _, _| {
                let file = (drag.path.clone(), drag.folder);
                Some(ExternalDragPayload::Files(FileDragPaths::new(vec![file])))
            })
        })
    }

    fn agent_row(&self, harness: Harness, cx: &mut Context<Self>) -> Div {
        let on = self.added.contains(&harness);
        let busy = self.busy == Some(harness);
        let failed = self
            .failed
            .as_ref()
            .filter(|(which, _)| *which == harness)
            .map(|(_, why)| why.clone());
        let subtitle = match failed {
            Some(why) => div().text_color(theme::danger()).child(why),
            None if busy => div().text_color(content(0.65)).child("Working…"),
            None if on => div().text_color(content(0.65)).child(harness.then()),
            None => div().text_color(content(0.65)).child(harness.adds()),
        };
        let (label, look) = match (on, self.busy.is_some()) {
            (_, true) => (if on { "Remove" } else { "Connect" }, ButtonLook::Disabled),
            (true, false) => ("Remove", ButtonLook::Ghost),
            (false, false) => ("Connect", ButtonLook::Secondary),
        };
        let toggle = self
            .button(
                harness.icon(),
                label,
                look,
                move |this, _, cx| this.toggle_agent(harness, cx),
                cx,
            )
            .aria_label(format!("{label} {}", harness.name()));
        let tile = div()
            .size(px(theme::SIDEBAR_ROW_HEIGHT - 6.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(theme::RADIUS_THUMB))
            .bg(content(0.08))
            .child(icon(harness.icon(), 16.).text_color(content(1.)));
        row(
            tile.into_any_element(),
            harness.name(),
            subtitle,
            div()
                .flex()
                .items_center()
                .gap_2()
                .when(on, |d| d.child(done("Connected")))
                .child(toggle)
                .into_any_element(),
        )
    }

    fn checklist(&self, cx: &mut Context<Self>) -> Div {
        let shown = self.agents.clone().unwrap_or_default();
        let empty = self.agents.as_ref().is_some_and(Vec::is_empty);
        let agents = shown
            .into_iter()
            .map(|harness| self.agent_row(harness, cx))
            .collect::<Vec<_>>();
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex_shrink_0()
                    .mb_2()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(logo(48.))
                    .child(title("Set up Monodrive")),
            )
            .child(
                div()
                    .id("checklist")
                    .min_h_0()
                    .overflow_y_scroll()
                    .rounded(px(theme::RADIUS_CARD))
                    .bg(content(0.03))
                    .child(self.access_row(cx))
                    .children(
                        agents
                            .into_iter()
                            .map(|agent| agent.border_t_1().border_color(content(0.06))),
                    )
                    .when(empty, |d| {
                        d.child(
                            div()
                                .p(px(theme::PANE_INSET))
                                .border_t_1()
                                .border_color(content(0.06))
                                .child(note(
                                    "No coding agents found. Settings ▸ Agents adds one later.",
                                )),
                        )
                    }),
            )
    }

    fn bare(&self) -> bool {
        self.app.extension().is_none_or(|ext| ext != "app")
    }

    fn footer(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex_shrink_0()
            .pt(px(theme::PANE_INSET))
            .flex()
            .items_center()
            .gap_2()
            .when(self.step == Step::Checklist, |d| {
                d.child(self.button(
                    "onboarding-back",
                    "Back",
                    ButtonLook::Secondary,
                    |this, _, cx| this.back(cx),
                    cx,
                ))
            })
            .child(div().flex_1())
            .child(match self.step {
                Step::Welcome => self.button(
                    "onboarding-primary",
                    "Continue",
                    ButtonLook::Primary,
                    |this, window, cx| this.primary(window, cx),
                    cx,
                ),
                // Never gated on the access: it's optional, and ↩ goes for it
                // first, so this is the way past it.
                Step::Checklist => self.button(
                    "onboarding-primary",
                    "Open Monodrive",
                    if self.access.granted {
                        ButtonLook::Primary
                    } else {
                        ButtonLook::Secondary
                    },
                    |this, window, cx| this.finish(window, cx),
                    cx,
                ),
            })
    }
}

fn logo(side: f32) -> Img {
    img(ImageSource::Resource(Resource::Embedded(LOGO.into())))
        .size(px(side))
        .flex_shrink_0()
        .role(Role::Image)
        .aria_label("The Monodrive icon")
}

fn title(text: &'static str) -> Div {
    div()
        .text_size(px(theme::TEXT_TITLE))
        .font_weight(FontWeight::SEMIBOLD)
        .child(text)
}

fn note(text: &'static str) -> Div {
    div()
        .text_size(px(theme::TEXT_CAPTION))
        .text_color(content(0.65))
        .child(text)
}

/// A checklist row's finished state.
fn done(text: &'static str) -> Div {
    div()
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap_1()
        .text_size(px(theme::TEXT_SMALL))
        .text_color(theme::success())
        .child(icon("check-hi", 14.).text_color(theme::success()))
        .child(text)
}

fn row(leading: AnyElement, name: &'static str, subtitle: Div, trailing: AnyElement) -> Div {
    div()
        .min_h(px(44.))
        .px(px(theme::PANE_INSET))
        .py_2()
        .flex()
        .items_center()
        .gap_3()
        .child(leading)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().text_size(px(theme::TEXT_BODY)).child(name))
                .child(subtitle.text_size(px(theme::TEXT_CAPTION))),
        )
        .child(trailing)
}

impl Render for Onboarding {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let prefs = cx.global::<Preferences>();
        let system_light = matches!(
            window.appearance(),
            WindowAppearance::Light | WindowAppearance::VibrantLight
        );
        let glass = theme::configure(prefs, system_light);
        if self.glass != Some(glass) {
            self.glass = Some(glass);
            window.set_background_appearance(theme::backdrop(glass));
        }
        let body = match self.step {
            Step::Welcome => self.welcome(cx),
            Step::Checklist => self.checklist(cx),
        };
        div()
            .key_context("Onboarding")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Continue, window, cx| {
                // A control the keys are on takes ↩ for itself.
                if this.focus.is_focused(window) {
                    this.primary(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &GoBack, window, cx| {
                window.focus(&this.focus, cx);
                this.back(cx);
            }))
            .on_action(|_: &CloseWelcome, window, _| window.remove_window())
            .on_action(|_: &FocusNext, window, cx| window.focus_next(cx))
            .on_action(|_: &FocusPrevious, window, cx| window.focus_prev(cx))
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme::UI_FONT)
            .text_color(content(1.))
            .bg(theme::shell_fill())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .flex()
                    .flex_col()
                    .bg(theme::pane_fill())
                    .child(browser::drag_area().h(px(theme::TITLE_BAR_HEIGHT)).w_full())
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .px_6()
                            .pb(px(theme::PANE_INSET))
                            .flex()
                            .flex_col()
                            .child(body)
                            .child(self.footer(cx)),
                    ),
            )
    }
}

#[cfg(test)]
pub(crate) mod tests;
