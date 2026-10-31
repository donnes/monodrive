//! The updater: a button left of Settings while a newer release is out, and a
//! popover listing what's new, with the button that installs it. One
//! `Updates` serves every window. Only a signed release checks, so debug
//! builds and tests never reach the network.

use crate::menu;
use crate::preferences::Preferences;
use crate::theme::{self, ButtonLook, content, danger, text_button};
use chrono::{DateTime, Utc};
use gpui::{App, Context, Entity, Global, KeyBinding, Role, Task, actions, div, prelude::*, px};
use monodrive_core::VERSION;
use monodrive_core::updates::{self, Channel, Release};
use std::path::PathBuf;
use std::time::Duration;

actions!(updates, [Dismiss]);

pub fn bind(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", Dismiss, Some("Updates"))]);
}

/// Often enough that a nightly shows the same day, and well inside GitHub's
/// 60 anonymous requests an hour.
const CHECK: Duration = Duration::from_secs(6 * 60 * 60);
const WIDTH: f32 = 360.;
/// The most releases the popover lists.
const SHOWN: usize = 5;

#[derive(Clone, Debug, PartialEq)]
enum Stage {
    Idle,
    Checking,
    Installing,
    /// Swapped in; the next launch runs it.
    Ready,
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
    _timer: Option<Task<()>>,
}

pub struct Shared(pub Entity<Updates>);
impl Global for Shared {}

/// Starts checking, at launch and every `CHECK`, and again when the channel
/// changes.
pub fn start(cx: &mut App) {
    let channel = cx.global::<Preferences>().update_channel;
    let updates = cx.new(|cx: &mut Context<Updates>| {
        let mut updates = Updates::new(channel, Vec::new(), None);
        updates._timer = Some(cx.spawn(async move |this, cx| {
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

/// Whether the sidebar shows the button.
pub fn available(cx: &App) -> bool {
    cx.try_global::<Shared>()
        .is_some_and(|shared| !shared.0.read(cx).pending.is_empty())
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
            _timer: None,
        }
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
        self.stage = Stage::Installing;
        self.error = None;
        cx.notify();
        self.work = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { updates::install(&release, &app) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.stage = Stage::Ready,
                    Err(error) => {
                        this.stage = Stage::Idle;
                        this.error = Some(error);
                    }
                }
                cx.notify();
            });
        }));
    }

    fn headline(&self) -> String {
        let newest = self.pending.first();
        match (&self.stage, newest) {
            (Stage::Ready, Some(release)) => {
                format!("Restart to finish updating to {}", release.version)
            }
            (Stage::Installing, Some(release)) => format!("Downloading {}…", release.version),
            (_, Some(_)) => "Update ready to download".into(),
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
    /// The popover's card; the browser places it and closes it.
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let action = match self.stage {
            Stage::Ready => Some(
                text_button("restart", "Restart", ButtonLook::Primary)
                    .on_click(|_, _, cx| cx.restart()),
            ),
            Stage::Installing => Some(text_button("download", "Download", ButtonLook::Disabled)),
            _ if !self.pending.is_empty() => Some(if self.error.is_some() {
                text_button("download", "Try Again", ButtonLook::Caution)
                    .on_click(cx.listener(|this, _, _, cx| this.install(cx)))
            } else {
                text_button("download", "Download", ButtonLook::Primary)
                    .on_click(cx.listener(|this, _, _, cx| this.install(cx)))
            }),
            _ => None,
        };
        lines.extend(action.map(|action| {
            div()
                .px_2()
                .py(px(6.))
                .flex()
                .justify_end()
                .child(action)
                .into_any_element()
        }));
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
