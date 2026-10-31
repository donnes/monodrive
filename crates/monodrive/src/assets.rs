use gpui::{AssetSource, Result, SharedString};
use std::borrow::Cow;

/// Icons compiled into the binary, named by file stem: `svg().path("search")`.
pub struct Assets;

macro_rules! icons {
    ($($name:literal),* $(,)?) => {
        const ICONS: &[(&str, &[u8])] = &[
            $(($name, include_bytes!(concat!("../assets/icons/", $name, ".svg")))),*
        ];
    };
}

icons!(
    "add-01",
    "agent-amp",
    "agent-antigravity",
    "agent-claude",
    "agent-codex",
    "agent-cursor",
    "agent-gemini",
    "agent-grok",
    "agent-opencode",
    "agent-pi",
    "app-window",
    "arrow-down-circle",
    "bot",
    "cancel-01",
    "check-hi",
    "chevron-down-hi",
    "chevron-left",
    "chevron-right",
    "clock",
    "cloud",
    "copy",
    "eject",
    "external-link",
    "eye",
    "file",
    "folder-01",
    "grip-vertical",
    "hard-drive",
    "keyboard",
    "link",
    "palette",
    "pencil",
    "rotate-ccw",
    "search",
    "server",
    "settings",
    "sidebar-left",
    "sliders",
    "terminal",
    "trash",
);

/// The app's mark in full color, for `img`.
pub const LOGO: &str = "logo";
/// The sky behind a nightly's sidebar, for `img`.
pub const NIGHT_SKY: &str = "night-sky";

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path == LOGO {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../../../assets/prod/logo.svg"
            ))));
        }
        if path == NIGHT_SKY {
            return Ok(Some(Cow::Borrowed(include_bytes!(
                "../../../assets/nightly/sidebar-sky.svg"
            ))));
        }
        Ok(ICONS
            .iter()
            .find(|(name, _)| *name == path)
            .map(|(_, bytes)| Cow::Borrowed(*bytes)))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| (*name).into())
            .collect())
    }
}
