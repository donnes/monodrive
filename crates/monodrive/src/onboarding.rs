//! First launch, in a window of its own: what Monodrive is, Full Disk Access,
//! coding agents, and the keys to know. It comes before the main windows and
//! before the first crawl, which would otherwise run without the access and
//! again after it. Show Welcome and the sidebar's notice open it later.

use crate::agents::{self, Action, Harness};
use crate::assets::LOGO;
use crate::browser::{self, QuickLook, SharedIndex};
use crate::commands::{self, OpenPalette};
use crate::preferences::{self, Preferences};
use crate::theme::{self, ButtonLook, content, icon, text_button};
use crate::traffic_lights;
use async_channel::{Receiver, Sender};
use gpui::{
    Action as _, App, ClickEvent, Context, Div, FocusHandle, Focusable, FontWeight, Global,
    ImageSource, KeyBinding, Resource, Role, Stateful, TitlebarOptions, Toggled, Window,
    WindowAppearance, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowId,
    WindowOptions, actions, div, img, point, prelude::*, px, size,
};
use monodrive_core::index::{self, Index};
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Raise it to show onboarding once more to everyone who finished this one.
pub const VERSION: u32 = 1;

const WIDTH: f32 = 760.;
const HEIGHT: f32 = 520.;
const RAIL_WIDTH: f32 = 200.;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    Welcome,
    FullDiskAccess,
    Agents,
    Ready,
}

impl Step {
    const ALL: [Step; 4] = [
        Step::Welcome,
        Step::FullDiskAccess,
        Step::Agents,
        Step::Ready,
    ];

    fn name(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome",
            Step::FullDiskAccess => "Full Disk Access",
            Step::Agents => "Connect Agents",
            Step::Ready => "Ready",
        }
    }

    fn previous(self) -> Option<Step> {
        Self::ALL.into_iter().rfind(|step| *step < self)
    }
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
    fn reveal(&self, path: &Path, cx: &App);
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

    fn reveal(&self, path: &Path, cx: &App) {
        cx.reveal_path(path);
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
    cx: &mut App,
) {
    let due = !explicit_folder && cx.global::<Preferences>().onboarding_version < VERSION;
    if !due {
        cx.set_global(IndexGate::new(setup, false));
        return then(cx);
    }
    let granted = setup.full_disk_access();
    let held = !granted && !setup.saved_index();
    cx.set_global(IndexGate::new(setup, held));
    // With the access there's nothing to explain, so it starts where there's
    // something to decide: an existing install, or a reopen after the grant.
    let step = if granted { Step::Agents } else { Step::Welcome };
    open(step, Some(Box::new(then)), cx);
    if granted {
        start_index(cx);
    }
    cx.activate(true);
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
/// goes ahead, before the app sees it has no windows left and quits.
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
    }
}

fn open_gate(cx: &App) {
    if let Some(gate) = cx.try_global::<IndexGate>() {
        gate.open();
    }
}

/// The access is settled, one way or the other, so the index can get going
/// without waiting for the rest of the steps.
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

pub struct Onboarding {
    step: Step,
    skipped: Vec<Step>,
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
            skipped: Vec::new(),
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
        if step == Step::Agents {
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

    /// What ↩ and the primary button do.
    fn primary(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.step {
            Step::Welcome => self.go(Step::FullDiskAccess, cx),
            Step::FullDiskAccess if self.access.granted => {
                self.skipped.retain(|step| *step != Step::FullDiskAccess);
                start_index(cx);
                self.go(Step::Agents, cx);
            }
            Step::FullDiskAccess => self.open_settings(cx),
            Step::Agents => {
                self.skipped.retain(|step| *step != Step::Agents);
                if self.added.is_empty() {
                    self.skipped.push(Step::Agents);
                }
                self.go(Step::Ready, cx);
            }
            Step::Ready => {
                preferences::update(cx, |p| p.onboarding_version = VERSION);
                window.remove_window();
            }
        }
    }

    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.asked = true;
        self.setup.open_privacy_settings(cx);
        cx.notify();
    }

    fn not_now(&mut self, cx: &mut Context<Self>) {
        if !self.skipped.contains(&Step::FullDiskAccess) {
            self.skipped.push(Step::FullDiskAccess);
        }
        start_index(cx);
        self.go(Step::Agents, cx);
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        if let Some(previous) = self.step.previous() {
            self.go(previous, cx);
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

    fn rail(&self) -> impl IntoElement {
        let rows = Step::ALL.into_iter().map(|step| {
            let current = step == self.step;
            let skipped = !current && self.skipped.contains(&step);
            let done = !current
                && !skipped
                && (step < self.step || (step == Step::FullDiskAccess && self.access.granted));
            let state = if current {
                "current"
            } else if skipped {
                "skipped"
            } else if done {
                "done"
            } else {
                "not yet"
            };
            div()
                .id(step.name())
                .role(Role::ListItem)
                .h(px(theme::SIDEBAR_ROW_HEIGHT))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded(px(theme::RADIUS_ROW))
                .aria_label(format!("{}, {state}", step.name()))
                .text_size(px(theme::TEXT_BODY))
                .text_color(content(if current || done { 1. } else { 0.65 }))
                .when(current, |d| {
                    d.bg(content(0.14)).font_weight(FontWeight::MEDIUM)
                })
                .child(
                    div()
                        .size(px(16.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .map(|d| {
                            if done {
                                d.child(icon("check-hi", 14.).text_color(theme::success()))
                            } else {
                                d.child(
                                    div()
                                        .size(px(8.))
                                        .rounded_full()
                                        .when(current, |d| d.bg(theme::accent()))
                                        .when(!current, |d| {
                                            d.border_1().border_color(content(0.3))
                                        }),
                                )
                            }
                        }),
                )
                .child(div().flex_1().min_w_0().truncate().child(step.name()))
                .when(skipped, |d| {
                    d.child(
                        div()
                            .text_size(px(theme::TEXT_CAPTION))
                            .text_color(content(0.65))
                            .child("Skipped"),
                    )
                })
        });
        div()
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .child(browser::drag_area().h(px(theme::TITLE_BAR_HEIGHT)).w_full())
            .child(
                div()
                    .id("onboarding-steps")
                    .role(Role::List)
                    .aria_label("Steps")
                    .px_2()
                    .flex()
                    .flex_col()
                    .gap(px(theme::ROW_GAP))
                    .children(rows),
            )
    }

    fn welcome(&self) -> Div {
        div().flex_1().flex().items_center().justify_center().child(
            img(ImageSource::Resource(Resource::Embedded(LOGO.into())))
                .size(px(96.))
                .role(Role::Image)
                .aria_label("The Monodrive icon"),
        )
    }

    fn access_step(&self, cx: &mut Context<Self>) -> Div {
        let bare = self.app.extension().is_none_or(|ext| ext != "app");
        let reveal = self.press(|this, _, cx| this.setup.reveal(&this.app, cx), cx);
        let state = if self.access.granted {
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(theme::TEXT_CAPTION))
                .text_color(theme::success())
                .debug_selector(|| "access-granted".into())
                .child(icon("check-hi", 14.).text_color(theme::success()))
                .child("Full Disk Access is on.")
                .into_any_element()
        } else {
            note(if bare {
                "This is a bare binary, so macOS asks about whichever app started it, such as a terminal. Use Monodrive.app to test the real thing."
            } else {
                "Not in the list? Click + under it and choose Monodrive, or drag it in from Finder."
            })
            .into_any_element()
        };
        // The index keeps skipping what it skipped when it started; macOS may
        // also hold a new grant back from a process that's already running.
        let reopen = if self.access.granted && self.access.index_lacks {
            Some("The index started without it. Reopen to include those folders.")
        } else if !self.access.granted && self.asked {
            Some("Already turned it on? macOS may apply it only after a reopen.")
        } else {
            None
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(theme::TEXT_SMALL))
                    .font_weight(FontWeight::MEDIUM)
                    .child("System Settings ▸ Privacy & Security ▸ Full Disk Access"),
            )
            .child(how_to())
            .child(
                div()
                    .h(px(theme::BUTTON_HEIGHT))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::MONO_FONT)
                            .text_size(px(theme::TEXT_CAPTION))
                            .text_color(content(0.65))
                            .debug_selector(|| "access-app".into())
                            .child(self.app.display().to_string()),
                    )
                    .child(
                        self.stop(
                            "access-reveal",
                            text_button("access-reveal", "Reveal in Finder", ButtonLook::Ghost),
                            cx,
                        )
                        .on_click(reveal),
                    ),
            )
            .child(state)
            .when_some(reopen, |d, why| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(div().flex_1().min_w_0().child(note(why)))
                        .child(self.button(
                            "access-reopen",
                            "Reopen Monodrive",
                            ButtonLook::Secondary,
                            |this, _, cx| this.setup.relaunch(cx),
                            cx,
                        )),
                )
            })
    }

    fn agents_step(&self, cx: &mut Context<Self>) -> Div {
        let shown = self.agents.clone().unwrap_or_default();
        let empty = self.agents.as_ref().is_some_and(Vec::is_empty);
        let rows = shown.into_iter().enumerate().map(|(ix, harness)| {
            let on = self.added.contains(&harness);
            let busy = self.busy == Some(harness);
            let failed = self
                .failed
                .as_ref()
                .filter(|(which, _)| *which == harness)
                .map(|(_, why)| why.clone());
            let subtitle = match &failed {
                Some(why) => div().text_color(theme::danger()).child(why.clone()),
                None if busy => div().text_color(content(0.65)).child("Working…"),
                None if on => div().text_color(content(0.65)).child(harness.then()),
                None => div().text_color(content(0.65)).child(harness.adds()),
            };
            let pressed = self.press(move |this, _, cx| this.toggle_agent(harness, cx), cx);
            let toggle = theme::switch(on)
                .id(("agent-switch", ix))
                .role(Role::Switch)
                .aria_label(harness.name())
                .aria_toggled(if on { Toggled::True } else { Toggled::False })
                .debug_selector(move || format!("agent-switch-{}", harness.name()))
                .when(!busy, |d| d.cursor_pointer())
                .on_click(pressed);
            div()
                .min_h(px(44.))
                .px(px(theme::PANE_INSET))
                .py_2()
                .flex()
                .items_center()
                .gap_3()
                .when(ix > 0, |d| d.border_t_1().border_color(content(0.06)))
                .child(icon(harness.icon(), 16.).text_color(content(1.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(div().text_size(px(theme::TEXT_BODY)).child(harness.name()))
                        .child(subtitle.text_size(px(theme::TEXT_CAPTION))),
                )
                .child(self.stop(harness.icon(), toggle, cx))
        });
        div().flex_1().min_h_0().flex().flex_col().child(
            div()
                .id("agents-list")
                .min_h_0()
                .overflow_y_scroll()
                .rounded(px(theme::RADIUS_CARD))
                .bg(content(0.03))
                .when(empty, |d| {
                    d.p(px(theme::PANE_INSET)).child(note(
                        "No coding agents found. Settings ▸ Agents adds one later.",
                    ))
                })
                .children(rows),
        )
    }

    fn ready_step(&self, cx: &App) -> Div {
        let first = |id: &str| {
            commands::shortcuts_of(id, cx)
                .first()
                .map(|keys| commands::label(keys))
        };
        // ⌘Y comes first for the menu bar; Space is the one to know.
        let quick_look = commands::shortcuts_of(QuickLook.name(), cx)
            .last()
            .map(|keys| commands::label(keys));
        let rows = [
            (
                first(OpenPalette.name()),
                "Command palette: every command, folder and file.",
            ),
            (quick_look, "Quick Look at what's selected."),
            (first(browser::NewTab.name()), "A new tab."),
        ];
        let keys = rows
            .into_iter()
            .filter_map(|(keys, what)| Some((keys?, what)));
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .rounded(px(theme::RADIUS_CARD))
                    .bg(content(0.03))
                    .children(keys.enumerate().map(|(ix, (keys, what))| {
                        div()
                            .min_h(px(44.))
                            .px(px(theme::PANE_INSET))
                            .flex()
                            .items_center()
                            .gap_3()
                            .when(ix > 0, |d| d.border_t_1().border_color(content(0.06)))
                            .child(
                                div()
                                    .w(px(64.))
                                    .flex_shrink_0()
                                    .flex()
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
                                    ),
                            )
                            .child(div().text_size(px(theme::TEXT_BODY)).child(what))
                    })),
            )
            .when(self.skipped.contains(&Step::FullDiskAccess), |d| {
                d.child(note(
                    "Full Disk Access is off, so Desktop, Documents and Mail are skipped. The sidebar's notice brings you back here.",
                ))
            })
    }

    fn footer(&self, cx: &mut Context<Self>) -> Div {
        let (label, look) = match self.step {
            Step::Welcome => ("Continue", ButtonLook::Primary),
            Step::FullDiskAccess if self.access.granted => ("Continue", ButtonLook::Primary),
            Step::FullDiskAccess => ("Open System Settings", ButtonLook::Primary),
            Step::Agents if self.added.is_empty() => ("Skip", ButtonLook::Primary),
            Step::Agents => ("Continue", ButtonLook::Primary),
            Step::Ready => ("Open Monodrive", ButtonLook::Primary),
        };
        div()
            .flex_shrink_0()
            .pt(px(theme::PANE_INSET))
            .flex()
            .items_center()
            .gap_2()
            .when(self.step.previous().is_some(), |d| {
                d.child(self.button(
                    "onboarding-back",
                    "Back",
                    ButtonLook::Secondary,
                    |this, _, cx| this.back(cx),
                    cx,
                ))
            })
            .child(div().flex_1())
            .when(
                self.step == Step::FullDiskAccess && !self.access.granted,
                |d| {
                    d.child(self.button(
                        "onboarding-not-now",
                        "Not Now",
                        ButtonLook::Secondary,
                        |this, _, cx| this.not_now(cx),
                        cx,
                    ))
                },
            )
            .child(self.button(
                "onboarding-primary",
                label,
                look,
                |this, window, cx| this.primary(window, cx),
                cx,
            ))
    }
}

fn note(text: &'static str) -> Div {
    div()
        .text_size(px(theme::TEXT_CAPTION))
        .text_color(content(0.65))
        .child(text)
}

/// System Settings as a picture: schematic, drawn here so it's crisp in both
/// themes and doesn't date with each macOS.
fn how_to() -> impl IntoElement {
    let place = |name: &'static str, here: bool| {
        div()
            .h(px(theme::BUTTON_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .rounded(px(theme::RADIUS_ROW))
            .text_size(px(theme::TEXT_CAPTION))
            .map(|d| {
                if here {
                    d.bg(theme::accent()).text_color(theme::on_accent())
                } else {
                    d.text_color(content(0.65))
                }
            })
            .child(name)
    };
    let app = |logo: Div, name: &'static str, on: bool, ink: f32| {
        div()
            .h(px(theme::SIDEBAR_ROW_HEIGHT))
            .px_2()
            .flex()
            .items_center()
            .gap_2()
            .child(logo.size(px(16.)).flex_shrink_0())
            .child(
                div()
                    .flex_1()
                    .text_size(px(theme::TEXT_CAPTION))
                    .text_color(content(ink))
                    .child(name),
            )
            .child(theme::switch(on))
    };
    div()
        .id("onboarding-how-to")
        .role(Role::Image)
        .aria_label(
            "System Settings, with Privacy & Security selected in the sidebar and \
             Full Disk Access open, listing Monodrive with its switch on.",
        )
        .flex_1()
        .min_h_0()
        .flex()
        .overflow_hidden()
        .rounded(px(theme::RADIUS_CARD))
        .border_1()
        .border_color(content(0.12))
        .bg(content(0.03))
        .child(
            div()
                .w(px(152.))
                .flex_shrink_0()
                .p_2()
                .flex()
                .flex_col()
                .gap(px(theme::ROW_GAP))
                .border_r_1()
                .border_color(content(0.06))
                .child(place("General", false))
                .child(place("Desktop & Dock", false))
                .child(place("Privacy & Security", true))
                .child(place("Touch ID & Password", false)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .p_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_size(px(theme::TEXT_SMALL))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(icon("chevron-left", 12.).text_color(content(0.55)))
                        .child("Full Disk Access"),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .rounded(px(theme::RADIUS_ROW))
                        .bg(content(0.03))
                        .border_1()
                        .border_color(content(0.06))
                        .child(app(
                            div().rounded(px(theme::RADIUS_THUMB)).bg(content(0.14)),
                            "Another app",
                            false,
                            0.65,
                        ))
                        .child(
                            app(
                                div().child(
                                    img(ImageSource::Resource(Resource::Embedded(LOGO.into())))
                                        .size(px(16.)),
                                ),
                                "Monodrive",
                                true,
                                1.,
                            )
                            .border_t_1()
                            .border_color(content(0.06))
                            .font_weight(FontWeight::MEDIUM),
                        ),
                )
                .child(
                    div().flex().gap_1().child(
                        div()
                            .size(px(20.))
                            .rounded_full()
                            .bg(content(0.08))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(icon("add-01", 12.).text_color(content(0.65))),
                    ),
                ),
        )
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
        let (title, about, body) = match self.step {
            Step::Welcome => (
                "Welcome to Monodrive",
                "A fast file manager that searches your whole disk as you type, for you and your coding agents.",
                self.welcome(),
            ),
            Step::FullDiskAccess => (
                "Full Disk Access",
                "Lets Monodrive search and browse folders like Desktop, Documents and Mail. Nothing leaves this Mac. Without it, those folders are skipped.",
                self.access_step(cx),
            ),
            Step::Agents => (
                "Connect Agents",
                "Connecting adds Monodrive to an agent's MCP servers, so it searches with the same index. Nothing changes until you turn one on.",
                self.agents_step(cx),
            ),
            Step::Ready => (
                "You're ready",
                "Three keys to start with.",
                self.ready_step(cx),
            ),
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
            .font_family(theme::UI_FONT)
            .text_color(content(1.))
            .bg(theme::shell_fill())
            .child(self.rail())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(theme::pane_fill())
                    .border_l_1()
                    .border_color(content(0.06))
                    .child(browser::drag_area().h(px(theme::TITLE_BAR_HEIGHT)).w_full())
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .px_6()
                            .pb(px(theme::PANE_INSET))
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_TITLE))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::TEXT_BODY))
                                    .text_color(content(0.65))
                                    .child(about),
                            )
                            .child(body.mt_2())
                            .child(self.footer(cx)),
                    ),
            )
    }
}

#[cfg(test)]
pub(crate) mod tests;
