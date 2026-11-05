//! The updater: a control left of Settings that checks for updates, or once
//! a newer release is out downloads it when pressed, and a card listing
//! what's new, which shows while the pointer is on the control and for Check
//! for Updates…. The control then rings with the download's progress and
//! counts down to the restart that runs it. One `Updates` serves every window. Only a
//! signed release checks, so debug builds and tests never reach the network.

use crate::browser::CheckForUpdates;
use crate::menu;
use crate::preferences::Preferences;
use crate::theme::{self, ButtonLook, accent, circle_button, content, danger, text_button};
use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, Global, KeyBinding, Role, Stateful, Task, Window,
    actions, div, prelude::*, px,
};
use monodrive_core::VERSION;
use monodrive_core::updates::{self, Channel, Release};
use std::path::PathBuf;
use std::time::Duration;

actions!(updates, [Dismiss]);

pub fn bind(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some("Updates"))]);
}

/// T3 Code's pace: the first check waits out the launch, then a release
/// shows within minutes. 15 checks an hour stay well inside GitHub's 60
/// anonymous requests. A short wait also means a Mac that slept, which
/// pauses timers, checks again soon after it wakes.
const FIRST_CHECK: Duration = Duration::from_secs(15);
const CHECK: Duration = Duration::from_secs(4 * 60);
/// The seconds the sidebar counts down before restarting into an update.
const COUNTDOWN: u8 = 5;
const WIDTH: f32 = 360.;
/// The most releases the popover lists.
const SHOWN: usize = 5;

#[derive(Clone, Debug, PartialEq)]
enum Stage {
    Idle,
    Checking,
    /// How much of the download has arrived, from 0 to 1. At 1 it's being
    /// checked and swapped in.
    Installing(f32),
    /// Swapped in; the next launch runs it. Holds the seconds left before
    /// the restart, or `None` once Later put it off.
    Ready(Option<u8>),
}

pub struct Updates {
    /// The bundle to replace, once known. `None` inside means this copy
    /// can't update itself.
    app: Option<Option<PathBuf>>,
    channel: Channel,
    pending: Vec<Release>,
    stage: Stage,
    error: Option<String>,
    work: Option<Task<()>>,
    countdown: Option<Task<()>>,
    _timer: Option<Task<()>>,
}

pub struct Shared(pub Entity<Updates>);
impl Global for Shared {}

/// Starts checking, soon after launch and every `CHECK`, and again when the
/// channel changes.
pub fn start(cx: &mut App) {
    let channel = cx.global::<Preferences>().update_channel;
    let updates = cx.new(|cx: &mut Context<Updates>| {
        let mut updates = Updates::new(channel, Vec::new(), None);
        updates._timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK).await;
            loop {
                if this.update(cx, |this, cx| this.check(cx)).is_err() {
                    return;
                }
                cx.background_executor().timer(CHECK).await;
            }
        }));
        updates
    });
    cx.observe_global::<Preferences>({
        let updates = updates.clone();
        move |cx| {
            let channel = cx.global::<Preferences>().update_channel;
            updates.update(cx, |updates, cx| {
                if updates.channel != channel {
                    updates.channel = channel;
                    updates.pending.clear();
                    updates.check(cx);
                }
            });
        }
    })
    .detach();
    cx.set_global(Shared(updates));
}

/// The sidebar's control left of Settings: a refresh icon that checks for
/// updates, a download icon with a dot while one is out, which downloads it,
/// a ring of the download's progress, a countdown to the restart, and a
/// restart icon once Later put it off. `hover` hears the pointer come and
/// go, so the browser can show the card.
pub fn control(
    hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Option<AnyElement> {
    let entity = cx.try_global::<Shared>()?.0.clone();
    let updates = entity.read(cx);
    let install = {
        let entity = entity.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            entity.update(cx, |updates, cx| updates.install(cx))
        }
    };
    let control = match updates.stage {
        Stage::Ready(Some(left)) => {
            let label = format!("Restart in {left}s");
            div()
                .flex_shrink_0()
                .h(px(theme::BUTTON_HEIGHT))
                .flex()
                .items_center()
                .rounded_full()
                .bg(content(0.08))
                .child(
                    div()
                        .id("updates-countdown")
                        .role(Role::Button)
                        .aria_label(label.clone())
                        .h_full()
                        .pl(px(12.))
                        .pr_1()
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(1.))
                        .child(label)
                        .on_click(|_, _, cx| cx.restart()),
                )
                .child(
                    text_button("updates-later", "Later", ButtonLook::Ghost).on_click(
                        move |_, _, cx| entity.update(cx, |updates, cx| updates.later(cx)),
                    ),
                )
                .into_any_element()
        }
        Stage::Ready(None) => marked("rotate-cw", "Restart to Update")
            .on_click(|_, _, cx| cx.restart())
            .into_any_element(),
        Stage::Installing(fraction) => {
            circle_button("updates", "Downloading Update", theme::BUTTON_HEIGHT)
                .aria_label(format!("Downloading Update, {:.0}%", fraction * 100.))
                .relative()
                .child(theme::ring(fraction, theme::BUTTON_HEIGHT))
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(theme::icon("download", 12.).text_color(content(1.))),
                )
                .into_any_element()
        }
        _ if !updates.has_update() => theme::tip(
            theme::icon_button("updates", "Check for Updates", "rotate-cw", true),
            "Check for Updates",
        )
        .on_click(|_, window, cx| window.dispatch_action(Box::new(CheckForUpdates), cx))
        .into_any_element(),
        _ => marked("download", "Update Available")
            .on_click(install)
            .into_any_element(),
    };
    // One id for every stage, so the pointer stays on it as the stage moves.
    Some(
        div()
            .id("updates-control")
            .flex_shrink_0()
            .on_hover(hover)
            .child(control)
            .into_any_element(),
    )
}

/// A chrome button with `icon` and an accent dot, which says it waits on
/// the user.
fn marked(icon: &'static str, label: &'static str) -> Stateful<gpui::Div> {
    circle_button("updates", label, theme::BUTTON_HEIGHT)
        .relative()
        .cursor_pointer()
        .hover(|s| s.bg(content(0.08)))
        .active(|s| s.bg(content(0.14)))
        .child(theme::icon(icon, 14.).text_color(content(1.)))
        .child(
            div()
                .absolute()
                .top(px(2.))
                .right(px(2.))
                .size(px(6.))
                .rounded_full()
                .bg(accent()),
        )
}

impl Updates {
    /// `app` is what `installed_app` found, or `None` while it's unknown.
    pub fn new(channel: Channel, pending: Vec<Release>, app: Option<Option<PathBuf>>) -> Self {
        Self {
            app,
            channel,
            pending,
            stage: Stage::Idle,
            error: None,
            work: None,
            countdown: None,
            _timer: None,
        }
    }

    /// Whether an update is out or on its way in.
    pub fn has_update(&self) -> bool {
        !self.pending.is_empty() || matches!(self.stage, Stage::Installing(_) | Stage::Ready(_))
    }

    /// Asks GitHub, unless an update is already on its way in.
    pub fn check(&mut self, cx: &mut Context<Self>) {
        if self.stage != Stage::Idle {
            return;
        }
        self.stage = Stage::Checking;
        self.error = None;
        cx.notify();
        let (known, channel) = (self.app.clone(), self.channel);
        self.work = Some(cx.spawn(async move |this, cx| {
            let (app, found) = cx
                .background_executor()
                .spawn(async move {
                    let app = known.unwrap_or_else(updates::installed_app);
                    let found = app.as_ref().map(|_| updates::pending(channel));
                    (app, found)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.app = Some(app);
                this.stage = Stage::Idle;
                match found {
                    Some(Ok(pending)) => this.pending = pending,
                    Some(Err(error)) => this.error = Some(error),
                    None => {}
                }
                cx.notify();
            });
        }));
    }

    fn install(&mut self, cx: &mut Context<Self>) {
        let (Some(release), Some(Some(app))) = (self.pending.first().cloned(), self.app.clone())
        else {
            return;
        };
        self.stage = Stage::Installing(0.);
        self.error = None;
        cx.notify();
        let (sender, progress) = async_channel::unbounded();
        self.work = Some(cx.spawn(async move |this, cx| {
            let install = cx.background_executor().spawn(async move {
                updates::install(&release, &app, |fraction| {
                    let _ = sender.try_send(fraction);
                })
            });
            // Ends when the install returns and drops the sender.
            while let Ok(fraction) = progress.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.stage = Stage::Installing(fraction);
                    cx.notify();
                });
            }
            let result = install.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.count_down(cx),
                    Err(error) => {
                        this.stage = Stage::Idle;
                        this.error = Some(error);
                    }
                }
                cx.notify();
            });
        }));
    }

    /// Restarts into the update in `COUNTDOWN` seconds, unless Later puts
    /// it off first.
    fn count_down(&mut self, cx: &mut Context<Self>) {
        self.stage = Stage::Ready(Some(COUNTDOWN));
        self.countdown = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let counting = this.update(cx, |this, cx| {
                    let Stage::Ready(Some(left)) = &mut this.stage else {
                        return false;
                    };
                    *left = left.saturating_sub(1);
                    if *left == 0 {
                        cx.restart();
                        return false;
                    }
                    cx.notify();
                    true
                });
                if !matches!(counting, Ok(true)) {
                    return;
                }
            }
        }));
    }

    /// Stops the countdown. The update waits for the next launch, or for
    /// the control's restart.
    fn later(&mut self, cx: &mut Context<Self>) {
        self.stage = Stage::Ready(None);
        self.countdown = None;
        cx.notify();
    }

    fn headline(&self) -> String {
        let newest = self.pending.first();
        match (&self.stage, newest) {
            (Stage::Ready(Some(left)), Some(release)) => {
                format!("Restarting in {left}s to update to {}", release.version)
            }
            (Stage::Ready(None), Some(release)) => {
                format!(
                    "Click the restart button to finish updating to {}",
                    release.version
                )
            }
            (Stage::Installing(fraction), Some(release)) if *fraction < 1. => {
                format!("Downloading {}… {:.0}%", release.version, fraction * 100.)
            }
            (Stage::Installing(_), Some(release)) => format!("Installing {}…", release.version),
            (_, Some(release)) => {
                format!("Click the update button to install {}", release.version)
            }
            (Stage::Checking, None) => "Checking for updates…".into(),
            (_, None) if self.app == Some(None) => {
                "This copy isn't a signed release, so it can't update itself.".into()
            }
            (_, None) if self.error.is_some() => "Couldn't check for updates.".into(),
            (_, None) => format!("Monodrive {VERSION} is up to date."),
        }
    }
}

/// How long ago `published` was, as "2h ago".
fn age(published: &str, now: DateTime<Utc>) -> Option<String> {
    let published = DateTime::parse_from_rfc3339(published).ok()?;
    let minutes = (now - published.with_timezone(&Utc)).num_minutes().max(0);
    Some(match minutes {
        0..60 => format!("{minutes}m ago"),
        60..1440 => format!("{}h ago", minutes / 60),
        _ => format!("{}d ago", minutes / 1440),
    })
}

impl Render for Updates {
    /// The card of what's new; the browser places it and closes it.
    fn render(&mut self, window: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        let now = Utc::now();
        let text = |text: String, size: f32, ink| {
            div()
                .px_2()
                .text_size(px(size))
                .text_color(ink)
                .child(text)
                .into_any_element()
        };
        let mut lines = vec![
            div()
                .px_2()
                .py(px(6.))
                .text_size(px(theme::TEXT_BODY))
                .font_weight(gpui::FontWeight::MEDIUM)
                .child(self.headline())
                .into_any_element(),
        ];
        lines.extend(self.error.clone().map(|error| {
            div()
                .px_2()
                .pb(px(6.))
                .text_size(px(theme::TEXT_SMALL))
                .text_color(danger())
                .child(error)
                .into_any_element()
        }));
        for (ix, release) in self.pending.iter().take(SHOWN).enumerate() {
            let when = age(&release.published, now)
                .map(|age| format!("{} · {age}", release.version))
                .unwrap_or_else(|| release.version.to_string());
            lines.push(text(when, theme::TEXT_CAPTION, content(0.65)));
            lines.extend(
                release
                    .notes
                    .highlights
                    .clone()
                    .map(|highlights| text(highlights, theme::TEXT_SMALL, content(1.))),
            );
            // Highlights say enough; the fixes wait on GitHub, as in T3 Code.
            if release.notes.highlights.is_none() {
                lines.extend(
                    release
                        .notes
                        .changes
                        .iter()
                        .take(SHOWN)
                        .map(|change| text(change.clone(), theme::TEXT_SMALL, content(1.))),
                );
            }
            let page = release.page.clone();
            let label = release
                .notes
                .stats
                .clone()
                .unwrap_or_else(|| "View on GitHub".into());
            lines.push(
                div()
                    .pb_2()
                    .flex()
                    .child(
                        theme::button(("release-page", ix), ButtonLook::Ghost)
                            .aria_label(format!("{label}, on GitHub"))
                            .child(label)
                            .child(theme::icon("external-link", 12.))
                            .on_click(move |_, _, cx| cx.open_url(&page)),
                    )
                    .into_any_element(),
            );
        }
        let height = window.viewport_size().height - px(theme::TITLE_BAR_HEIGHT + 16.);
        menu::card(WIDTH, height, lines)
            .id("updates")
            .role(Role::Dialog)
            .aria_label("Updates")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_counts_minutes_hours_and_days() {
        let now = DateTime::parse_from_rfc3339("2026-10-31T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let ago = |at: &str| age(at, now);
        assert_eq!(ago("2026-10-31T11:58:00Z").as_deref(), Some("2m ago"));
        assert_eq!(ago("2026-10-31T10:00:00Z").as_deref(), Some("2h ago"));
        assert_eq!(ago("2026-10-29T12:00:00Z").as_deref(), Some("2d ago"));
        assert_eq!(ago(""), None);
    }
}
