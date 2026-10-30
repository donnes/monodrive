//! What Settings edits. It lives in a global and is saved as JSON in
//! Monodrive's support folder.

use crate::background::{Effect, Scope};
use gpui::{App, Global, Task};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    System,
    Dark,
    Light,
}

/// The size a new window opens at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowSize {
    /// 60% of the screen each way, at most 1025 × 900, centered, as
    /// Tinycast's Reasonable Size.
    Reasonable,
    /// 90% of the screen each way, centered.
    AlmostFull,
    /// Where and how big the window was last.
    LastUsed,
}

/// A window frame in screen points, as saved for `WindowSize::LastUsed`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// How tall list rows are; their icons and text scale with them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowSize {
    Small,
    Medium,
    Large,
}

impl RowSize {
    /// Row height, icon size and text size, in points.
    pub fn metrics(self) -> (f32, f32, f32) {
        match self {
            RowSize::Small => (24., 14., 12.),
            RowSize::Medium => (28., 16., 13.),
            RowSize::Large => (34., 20., 14.),
        }
    }
}

/// How a folder's files are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    List,
    Grid,
}

/// Grid icon sizes, in points.
pub const GRID_ICON_SIZES: std::ops::RangeInclusive<u32> = 48..=160;

/// Rounded insets the file list as a card; Boxy runs it full height.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkspaceStyle {
    Rounded,
    Boxy,
}

/// Monocode's interface scale steps.
pub const UI_SCALE_PERCENTS: [u32; 16] = [
    50, 60, 70, 80, 90, 100, 110, 120, 130, 140, 150, 160, 170, 180, 190, 200,
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub show_hidden: bool,
    pub theme: Theme,
    /// A preset from `palettes::PALETTES`.
    pub palette: String,
    /// `None` uses the palette's accent.
    pub accent: Option<u32>,
    pub hue: u32,
    pub saturation: u32,
    pub dark_lightness: u32,
    pub sidebar_opacity: u32,
    pub main_pane_glass: bool,
    /// Monodrive's own copy of the chosen image.
    pub background: Option<PathBuf>,
    pub background_effect: Effect,
    pub background_scope: Scope,
    /// Percent opacity in an empty folder.
    pub background_empty: u32,
    /// Percent opacity in a folder with items.
    pub background_folder: u32,
    pub workspace_style: WorkspaceStyle,
    /// Percent; one of `UI_SCALE_PERCENTS`.
    pub ui_scale: u32,
    /// Points, within `SIDEBAR_WIDTHS`.
    pub sidebar_width: f32,
    /// Shortcuts changed on the Keyboard page, by command id.
    pub shortcuts: crate::commands::Shortcuts,
    /// How folders are listed.
    pub sort: monodrive_core::entries::Sort,
    pub window_size: WindowSize,
    /// The last window frame, kept while `window_size` is `LastUsed`.
    pub window_frame: Option<Frame>,
    pub row_size: RowSize,
    pub view: View,
    /// Within `GRID_ICON_SIZES`.
    pub grid_icon: u32,
    /// The grid's names, sized as list rows are.
    pub grid_label: RowSize,
    /// Images show their pictures in the grid.
    pub thumbnails: bool,
    /// Agents may change files over MCP. The operation owner reads it from
    /// the saved file, by this name.
    pub agents_change_files: bool,
}

/// How narrow and wide the sidebar can be dragged.
pub const SIDEBAR_WIDTHS: std::ops::RangeInclusive<f32> = 160.0..=400.0;

impl Default for Preferences {
    /// Monocode's defaults: a neutral dark glass shell.
    fn default() -> Self {
        Self {
            show_hidden: false,
            theme: Theme::Dark,
            palette: "default".into(),
            accent: None,
            hue: 240,
            saturation: 0,
            dark_lightness: 9,
            sidebar_opacity: 85,
            main_pane_glass: true,
            background: None,
            background_effect: Effect::None,
            background_scope: Scope::All,
            background_empty: 24,
            background_folder: 24,
            workspace_style: WorkspaceStyle::Rounded,
            ui_scale: 100,
            sidebar_width: 240.,
            shortcuts: Default::default(),
            sort: Default::default(),
            window_size: WindowSize::Reasonable,
            window_frame: None,
            row_size: RowSize::Medium,
            view: View::List,
            grid_icon: 72,
            grid_label: RowSize::Medium,
            thumbnails: true,
            agents_change_files: true,
        }
    }
}

impl Global for Preferences {}

impl Preferences {
    pub fn restore_window(&mut self) {
        let defaults = Self::default();
        self.window_size = defaults.window_size;
        self.window_frame = defaults.window_frame;
        self.row_size = defaults.row_size;
        self.view = defaults.view;
        self.grid_icon = defaults.grid_icon;
        self.grid_label = defaults.grid_label;
        self.thumbnails = defaults.thumbnails;
    }

    pub fn restore_general(&mut self) {
        let defaults = Self::default();
        self.show_hidden = defaults.show_hidden;
        self.sort = defaults.sort;
    }

    /// Everything on the Appearance page. What other pages own, and the
    /// sidebar's width, stay; a setting added outside Appearance belongs here.
    pub fn restore_appearance(&mut self) {
        *self = Self {
            show_hidden: self.show_hidden,
            sidebar_width: self.sidebar_width,
            shortcuts: std::mem::take(&mut self.shortcuts),
            sort: self.sort,
            window_size: self.window_size,
            window_frame: self.window_frame,
            row_size: self.row_size,
            view: self.view,
            grid_icon: self.grid_icon,
            grid_label: self.grid_label,
            thumbnails: self.thumbnails,
            agents_change_files: self.agents_change_files,
            ..Self::default()
        };
    }

    /// The background's opacity in a folder, or 0 where it doesn't show.
    pub fn background_opacity(&self, empty: bool) -> f32 {
        let percent = match (self.background_scope, empty) {
            (Scope::Empty, false) => 0,
            (_, true) => self.background_empty,
            (_, false) => self.background_folder,
        };
        percent.min(100) as f32 / 100.
    }

    /// The next interface scale step up or down, or 100% for `0`.
    pub fn step_ui_scale(&mut self, step: i32) {
        self.ui_scale = match step {
            0 => 100,
            1 => UI_SCALE_PERCENTS
                .into_iter()
                .find(|&p| p > self.ui_scale)
                .unwrap_or(self.ui_scale),
            _ => UI_SCALE_PERCENTS
                .into_iter()
                .rev()
                .find(|&p| p < self.ui_scale)
                .unwrap_or(self.ui_scale),
        };
    }

    /// Picks a preset and moves the color sliders to its surface tint.
    pub fn apply_palette(&mut self, id: &str) {
        // design: reads the preset's own color, not one the UI paints.
        let surface: gpui::Hsla = gpui::rgb(crate::palettes::palette(id, false).surface).into();
        self.hue = (surface.h * 360.).round() as u32;
        self.saturation = (surface.s * 100.).round() as u32;
        self.dark_lightness = ((surface.l * 100.).round() as u32).min(30);
        self.palette = id.into();
    }
}

fn path() -> Option<PathBuf> {
    Some(monodrive_core::support_dir()?.join("settings.json"))
}

/// Reads saved preferences. A missing or unreadable file means defaults, and
/// fields an older file lacks take theirs.
pub fn load() -> Preferences {
    path()
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Where preferences are saved. `init` sets it; without it, as in tests
/// and benchmarks, changes stay in memory and never touch the user's file.
struct Store(PathBuf);
impl Global for Store {}

fn save(path: &Path, prefs: &Preferences) {
    let Ok(json) = serde_json::to_vec_pretty(prefs) else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Write then rename, so a crash mid-write never leaves half a file.
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

/// The save waiting out a burst of changes, such as a slider drag.
struct PendingSave(#[allow(dead_code)] Task<()>);
impl Global for PendingSave {}

const SAVE_DELAY: Duration = Duration::from_millis(300);

pub fn init(cx: &mut App) {
    cx.set_global(load());
    let Some(path) = path() else { return };
    cx.set_global(Store(path));
    // A save still waiting when the app quits would be lost.
    cx.on_app_quit(|cx| {
        let prefs = cx.global::<Preferences>().clone();
        let path = cx.global::<Store>().0.clone();
        async move { save(&path, &prefs) }
    })
    .detach();
}

/// Changes preferences, repaints whatever observes them, and saves shortly
/// after, off the UI thread.
pub fn update(cx: &mut App, change: impl FnOnce(&mut Preferences)) {
    let mut prefs = cx.global::<Preferences>().clone();
    change(&mut prefs);
    if prefs == *cx.global::<Preferences>() {
        return;
    }
    cx.set_global(prefs.clone());
    let Some(path) = cx.try_global::<Store>().map(|store| store.0.clone()) else {
        return;
    };
    let timer = cx.background_executor().timer(SAVE_DELAY);
    // Replacing the pending save drops it, so only the last change is written.
    let task = cx.background_executor().spawn(async move {
        timer.await;
        save(&path, &prefs);
    });
    cx.set_global(PendingSave(task));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoring_appearance_leaves_other_pages_alone() {
        let mut prefs = Preferences {
            show_hidden: true,
            hue: 30,
            ui_scale: 150,
            sidebar_width: 300.,
            shortcuts: [("browser::Back".to_string(), Some("cmd-b".to_string()))].into(),
            ..Default::default()
        };
        let kept = (
            prefs.show_hidden,
            prefs.sidebar_width,
            prefs.shortcuts.clone(),
        );
        prefs.restore_appearance();
        assert_eq!(
            (prefs.show_hidden, prefs.sidebar_width, prefs.shortcuts),
            kept
        );
        assert_eq!((prefs.hue, prefs.ui_scale), (240, 100));
    }
}
