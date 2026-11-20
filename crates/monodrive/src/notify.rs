//! What Monodrive says in macOS's notifications, each kind behind its own
//! switch in Settings › Notifications. The window says the same in toasts and
//! pages, so nothing posts while it's in front, nor while an agent runs it.

use crate::browser::{self, Browser};
use crate::frame;
use crate::localsend;
use crate::preferences::Preferences;
use crate::session::Shows;
use gpui::{App, SystemNotification, SystemNotificationAction};
use monodrive_core::overview::{Headline, Suggestion};
use objc2_foundation::NSBundle;
use objc2_user_notifications::{
    UNAuthorizationStatus, UNNotificationSettings, UNUserNotificationCenter,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::ptr::NonNull;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    LocalSendOffer,
    LocalSendEnded,
    LowDisk,
    Worktrees,
}

/// A storage notice says so once, then keeps quiet this long.
const QUIET: u64 = 7 * 24 * 60 * 60;

const LOW_DISK: &str = "low-disk";
const WORKTREES: &str = "worktrees";

/// Settings want this kind said, and no agent is running the app.
pub fn wanted(kind: Kind, cx: &App) -> bool {
    let p = cx.global::<Preferences>();
    !frame::unattended()
        && p.notifications
        && match kind {
            Kind::LocalSendOffer => p.notify_localsend_offers,
            Kind::LocalSendEnded => p.notify_localsend_ended,
            Kind::LowDisk => p.notify_low_disk,
            Kind::Worktrees => p.notify_worktrees,
        }
}

/// Posts it if it's wanted and another app is in front. Says whether it did.
pub fn post(kind: Kind, tag: String, body: String, actions: &[(&str, &str)], cx: &App) -> bool {
    if !wanted(kind, cx) || frame::in_front() {
        return false;
    }
    cx.show_system_notification(SystemNotification {
        tag: tag.into(),
        title: match kind {
            Kind::LocalSendOffer | Kind::LocalSendEnded => "LocalSend",
            Kind::LowDisk | Kind::Worktrees => "Monodrive",
        }
        .into(),
        body: body.into(),
        actions: actions
            .iter()
            .map(|(id, label)| SystemNotificationAction {
                id: (*id).to_owned().into(),
                label: (*label).to_owned().into(),
            })
            .collect(),
    });
    true
}

/// Sends a press on a notification where it belongs. GPUI takes one handler
/// for the whole app.
pub fn start(cx: &mut App) {
    cx.on_system_notification_response(|response, cx| {
        let action = response.action_id.as_deref();
        match response.tag.as_ref() {
            LOW_DISK => show(Shows::Overview, cx),
            WORKTREES => show(Shows::Worktrees, cx),
            tag => {
                if !localsend::respond(tag, action, cx) {
                    frame::bring_forward(cx);
                }
            }
        }
    });
}

/// Brings the window in front forward, showing `page`.
fn show(page: Shows, cx: &mut App) {
    let front = cx
        .window_stack()
        .into_iter()
        .flatten()
        .chain(cx.windows())
        .find_map(|window| window.downcast::<Browser>());
    if let Some(window) = front {
        let _ = window.update(cx, |browser, window, cx| browser.show(page, window, cx));
    }
    frame::bring_forward(cx);
}

/// What the storage watch found, off the main thread.
pub struct Storage {
    /// The startup disk, and what the Overview's suggestions and the Trash
    /// would give back, while it's low.
    pub low: Option<(Headline, u64)>,
    /// The clean worktrees, as one suggestion.
    pub worktrees: Option<Suggestion>,
    pub said: Said,
    /// Whether macOS shows Monodrive's notifications yet.
    pub allowed: bool,
}

/// When each storage notice was last said, in seconds since 1970, kept in
/// the support folder so a relaunch doesn't say it again.
#[derive(Default, Serialize, Deserialize)]
pub struct Said {
    #[serde(default)]
    low_disk: Option<u64>,
    #[serde(default)]
    worktrees: Option<u64>,
}

impl Said {
    fn path() -> Option<PathBuf> {
        Some(monodrive_core::support_dir()?.join("notified.json"))
    }

    pub fn read() -> Said {
        Said::path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(path) = Said::path()
            && let Ok(json) = serde_json::to_vec(self)
        {
            let _ = std::fs::write(path, json);
        }
    }
}

/// Whether macOS shows Monodrive's notifications. It's no until the person
/// answers macOS's question, which the first notification asks. Waits for
/// macOS, so it runs off the main thread.
pub fn allowed() -> bool {
    // The notification center aborts a process outside an app bundle.
    if NSBundle::mainBundle().bundleIdentifier().is_none() {
        return false;
    }
    let (tell, told) = std::sync::mpsc::channel();
    let answer = block2::RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
        // SAFETY: macOS passes settings that live through the call.
        let _ = tell.send(unsafe { settings.as_ref() }.authorizationStatus());
    });
    UNUserNotificationCenter::currentNotificationCenter()
        .getNotificationSettingsWithCompletionHandler(&answer);
    told.recv().is_ok_and(|status| {
        status == UNAuthorizationStatus::Authorized || status == UNAuthorizationStatus::Provisional
    })
}

/// Says what `found` is worth saying, a week apart at most, and returns when
/// each was said if that changed. A notice macOS doesn't show yet is said
/// again on the next watch.
pub fn storage(found: Storage, now: u64, cx: &App) -> Option<Said> {
    let over_gb = cx.global::<Preferences>().notify_worktrees_over;
    let notices = notices(&found, over_gb, &found.said, now);
    let mut said = found.said;
    let mut changed = false;
    for notice in notices {
        if post(notice.kind, notice.tag.into(), notice.body, &[], cx) && found.allowed {
            match notice.kind {
                Kind::LowDisk => said.low_disk = Some(now),
                _ => said.worktrees = Some(now),
            }
            changed = true;
        }
    }
    changed.then_some(said)
}

struct Notice {
    kind: Kind,
    tag: &'static str,
    body: String,
}

/// What's worth saying: a low disk, and clean worktrees past `over_gb`, each
/// unless it was said within the week.
fn notices(found: &Storage, over_gb: u32, said: &Said, now: u64) -> Vec<Notice> {
    let quiet = |last: Option<u64>| last.is_some_and(|last| now < last.saturating_add(QUIET));
    let mut notices = Vec::new();
    if let Some((disk, freeable)) = &found.low
        && !quiet(said.low_disk)
    {
        let free = browser::format_bytes(disk.free);
        let body = match freeable {
            0 => format!("{} has {free} free.", disk.name),
            bytes => format!(
                "{} has {free} free. Monodrive found {} you can free in Overview.",
                disk.name,
                browser::format_bytes(*bytes)
            ),
        };
        notices.push(Notice {
            kind: Kind::LowDisk,
            tag: LOW_DISK,
            body,
        });
    }
    if let Some(worktrees) = &found.worktrees
        && worktrees.size.unshared >= u64::from(over_gb) * 1_000_000_000
        && !quiet(said.worktrees)
    {
        let size = browser::format_bytes(worktrees.size.unshared);
        let size = if worktrees.size.complete {
            size
        } else {
            format!("at least {size}")
        };
        notices.push(Notice {
            kind: Kind::Worktrees,
            tag: WORKTREES,
            body: format!(
                "{} take {size}. Git says they hold nothing their repositories don't.",
                localsend::count(worktrees.worktrees().len(), "clean worktree"),
            ),
        });
    }
    notices
}

#[cfg(test)]
mod tests {
    use super::*;
    use monodrive_core::overview::{Evidence, Measured};

    const NOW: u64 = 1_800_000_000;
    const GB: u64 = 1_000_000_000;

    fn found(free: u64, worktrees: u64) -> Storage {
        let disk = Headline {
            name: "Macintosh HD".into(),
            total: 500 * GB,
            free,
            trash: None,
            system: None,
            low: true,
        };
        let size = Measured {
            on_disk: worktrees,
            unshared: worktrees,
            at: NOW,
            complete: true,
        };
        Storage {
            low: Some((disk, 12 * GB)),
            worktrees: Some(Suggestion {
                evidence: Evidence::Worktrees {
                    worktrees: vec!["/a".into(), "/b".into()],
                    repositories: vec!["app".into()],
                },
                folders: Vec::new(),
                size,
            }),
            said: Said::default(),
            allowed: true,
        }
    }

    #[test]
    fn storage_is_said_past_its_line_then_not_again_for_a_week() {
        let kinds = |notices: Vec<Notice>| notices.iter().map(|n| n.kind).collect::<Vec<_>>();
        let said = Said::default();
        let first = notices(&found(40 * GB, 24 * GB), 20, &said, NOW);
        assert_eq!(
            first[0].body,
            "Macintosh HD has 40.00 GB free. Monodrive found 12.00 GB you can free in Overview."
        );
        assert_eq!(
            first[1].body,
            "2 clean worktrees take 24.00 GB. Git says they hold nothing their repositories don't."
        );
        assert_eq!(
            kinds(notices(&found(40 * GB, 19 * GB), 20, &said, NOW)),
            [Kind::LowDisk],
            "under the line it picked"
        );

        let said = Said {
            low_disk: Some(NOW),
            worktrees: Some(NOW),
        };
        let day = 24 * 60 * 60;
        assert!(notices(&found(40 * GB, 24 * GB), 20, &said, NOW + 6 * day).is_empty());
        assert_eq!(
            kinds(notices(&found(40 * GB, 24 * GB), 20, &said, NOW + 7 * day)),
            [Kind::LowDisk, Kind::Worktrees]
        );
    }
}
