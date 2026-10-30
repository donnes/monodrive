//! Monocode's appearance tokens: a glass shell with content ink drawn at
//! varying alpha instead of fixed greys. [`configure`] sets them from the
//! preferences before each frame; views read them through these functions.

use crate::palettes;
use crate::preferences::{Preferences, Theme};
use gpui::{
    BoxShadow, Div, ElementId, FontWeight, Hsla, Role, SharedString, Stateful, Svg,
    WindowBackgroundAppearance, canvas, div, prelude::*, px, rgb, svg,
};
use std::cell::Cell;

pub const UI_FONT: &str = ".SystemUIFont";
pub const MONO_FONT: &str = "Menlo";

// The scales DESIGN.md describes; scripts/check-design holds views to them.
pub const RADIUS_THUMB: f32 = 4.;
pub const RADIUS_ROW: f32 = 8.;
pub const RADIUS_CARD: f32 = 14.;

pub const TEXT_CAPTION: f32 = 11.;
pub const TEXT_SMALL: f32 = 12.;
pub const TEXT_BODY: f32 = 13.;
pub const TEXT_TITLE: f32 = 14.;

pub const TITLE_BAR_HEIGHT: f32 = 40.;
pub const ROW_HEIGHT: f32 = 28.;
pub const SIDEBAR_ROW_HEIGHT: f32 = 32.;
/// Under each stacked row, so highlights never touch.
pub const ROW_GAP: f32 = 2.;
/// The group a [`row_slot`] hovers its row through.
pub const ROW_GROUP: &str = "row";
pub const PATH_BAR_HEIGHT: f32 = 22.;
pub const BUTTON_HEIGHT: f32 = 24.;
pub const FIELD_HEIGHT: f32 = 28.;

#[derive(Clone, Copy, PartialEq)]
struct Tokens {
    light: bool,
    ink: Hsla,
    accent: Hsla,
    shell: Hsla,
    pane: Hsla,
}

thread_local! {
    static TOKENS: Cell<Option<Tokens>> = const { Cell::new(None) };
}

fn tokens() -> Tokens {
    TOKENS
        .get()
        .unwrap_or_else(|| derive(&Preferences::default(), false))
}

fn derive(p: &Preferences, system_light: bool) -> Tokens {
    let light = match p.theme {
        Theme::Light => true,
        Theme::Dark => false,
        Theme::System => system_light,
    };
    let palette = palettes::palette(&p.palette, light);
    // Light themes paint opaque, as Monocode's do.
    let glass = !light;
    let shell = gpui::hsla(
        p.hue.min(360) as f32 / 360.,
        p.saturation.min(100) as f32 / 100.,
        if light {
            0.97
        } else {
            p.dark_lightness.min(30) as f32 / 100.
        },
        if glass {
            p.sidebar_opacity.clamp(15, 100) as f32 / 100.
        } else {
            1.
        },
    );
    // A glass content pane shows the window's tint as it is; a solid one is
    // a deeper dark, or white in light themes.
    let mut pane = shell;
    pane.l = if light { 1. } else { shell.l * 0.45 };
    pane.a = if glass && p.main_pane_glass { 0. } else { 1. };
    Tokens {
        light,
        ink: rgb(palette.content).into(),
        accent: rgb(p.accent.unwrap_or(palette.accent)).into(),
        shell,
        pane,
    }
}

/// Sets the tokens views paint with. Returns whether the window shows the
/// blurred desktop behind it.
pub fn configure(p: &Preferences, system_light: bool) -> bool {
    let tokens = derive(p, system_light);
    TOKENS.set(Some(tokens));
    !tokens.light
}

pub fn backdrop(glass: bool) -> WindowBackgroundAppearance {
    if glass {
        WindowBackgroundAppearance::Blurred
    } else {
        WindowBackgroundAppearance::Opaque
    }
}

pub fn is_light() -> bool {
    tokens().light
}

/// The theme's content ink at `alpha`.
pub fn content(alpha: f32) -> Hsla {
    tokens().ink.opacity(alpha)
}

pub fn accent() -> Hsla {
    tokens().accent
}

/// Red ink for what failed or can't be undone, at 4.5:1 on the theme's panes.
pub fn danger() -> Hsla {
    rgb(if is_light() { 0xdc2626 } else { 0xff453a }).into()
}

/// Green ink for what's done.
pub fn success() -> Hsla {
    rgb(if is_light() { 0x059669 } else { 0x10b981 }).into()
}

/// Black or white, whichever reads on the accent.
pub fn on_accent() -> Hsla {
    let rgba = accent().to_rgb();
    let linear = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    let luminance = 0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b);
    if luminance > 0.179 {
        gpui::black()
    } else {
        gpui::white()
    }
}

/// The window's surface over the blurred desktop.
pub fn shell_fill() -> Hsla {
    tokens().shell
}

/// The content pane: clear over the window's glass, or solid.
pub fn pane_fill() -> Hsla {
    tokens().pane
}

fn black(alpha: f32) -> Hsla {
    gpui::hsla(0., 0., 0., alpha)
}

/// What dims the window behind a sheet, at `strength` of its full depth while
/// it fades in.
pub fn scrim(strength: f32) -> Hsla {
    black(if is_light() { 0.15 } else { 0.3 } * strength)
}

/// The sunken fill behind code.
pub fn well() -> Hsla {
    black(if is_light() { 0.04 } else { 0.2 })
}

/// A switch's or a slider's knob, white in every theme.
pub fn knob() -> Hsla {
    gpui::white()
}

/// A selected segment's fill, raised off its track.
pub fn raised() -> Hsla {
    if is_light() { knob() } else { content(0.2) }
}

/// The keyboard focus ring: the accent, just outside the control's shape.
pub fn focus_ring() -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(0.), accent().opacity(0.8)).spread_radius(px(2.5))]
}

pub fn knob_shadow() -> Vec<BoxShadow> {
    vec![
        BoxShadow::new(px(0.), px(1.), black(0.3)).blur_radius(px(3.)),
        BoxShadow::new(px(0.), px(0.), black(0.1)).spread_radius(px(0.5)),
    ]
}

pub fn raised_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow::new(px(0.), px(1.), black(0.15)).blur_radius(px(2.))]
}

/// How far a floating surface sits above the window.
#[derive(Clone, Copy)]
pub enum Elevation {
    /// Menus, popovers and toasts, near what opened them.
    Popover,
    /// Sheets over a scrim.
    Sheet,
}

/// A floating surface: the card radius, a hairline, a shadow and the frosted
/// fill. Its contents need `relative()` to draw over the fill.
pub fn floating(elevation: Elevation) -> Div {
    let shadow = match elevation {
        Elevation::Popover => BoxShadow::new(px(0.), px(8.), black(0.25))
            .blur_radius(px(24.))
            .spread_radius(px(-4.)),
        Elevation::Sheet => BoxShadow::new(px(0.), px(16.), black(0.3))
            .blur_radius(px(40.))
            .spread_radius(px(-8.)),
    };
    div()
        .relative()
        .rounded(px(RADIUS_CARD))
        .border_1()
        .border_color(content(0.12))
        .shadow(vec![shadow])
        .text_color(content(1.))
        .child(floating_surface())
}

/// A frosted fill, or an opaque one in light themes.
fn floating_surface() -> impl IntoElement {
    // Inside the surface's border.
    let radius = RADIUS_CARD - 1.;
    let Tokens {
        light, shell, pane, ..
    } = tokens();
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let quad = if light {
                gpui::fill(bounds, Hsla { a: 1., ..pane })
            } else {
                // Monocode's renderer patch reads this border color as a
                // request to blur what is behind the quad.
                gpui::fill(bounds, Hsla { a: 0.6, ..shell })
                    .border_color(gpui::backdrop_blur_marker(24. * window.scale_factor()))
            };
            window.paint_quad(quad.corner_radii(px(radius)));
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

pub fn section_label(label: &'static str) -> Div {
    div()
        .h(px(28.))
        .px_2()
        .mt_2()
        .flex()
        .items_center()
        .text_size(px(TEXT_SMALL))
        .font_weight(FontWeight::MEDIUM)
        .text_color(content(0.65))
        .child(label)
}

pub fn icon(path: &'static str, side: f32) -> Svg {
    svg()
        .path(path)
        .size(px(side))
        .text_color(content(0.5))
        .flex_shrink_0()
}

/// What a text button looks like.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonLook {
    Secondary,
    /// What ↩ does.
    Primary,
    /// What ↩ does, and it can't be undone.
    Destructive,
    /// Leads to something that can't be undone, or recovers from a failure.
    Caution,
    /// No fill until hovered, for a button among text.
    Ghost,
    /// It still says what it would do.
    Disabled,
}

/// A text button's frame without its label: a 24 pt pill in `look`. For a
/// button with an icon or a changing label; else use [`text_button`].
pub fn button(id: impl Into<ElementId>, look: ButtonLook) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .h(px(BUTTON_HEIGHT))
        .px(px(if look == ButtonLook::Ghost { 8. } else { 12. }))
        .flex_shrink_0()
        .rounded_full()
        .flex()
        .items_center()
        .justify_center()
        .gap_1()
        .text_size(px(TEXT_SMALL))
        .font_weight(match look {
            ButtonLook::Primary | ButtonLook::Destructive => FontWeight::MEDIUM,
            _ => FontWeight::NORMAL,
        })
        .when(look != ButtonLook::Disabled, |d| d.cursor_pointer())
        .map(|d| match look {
            ButtonLook::Secondary => d
                .bg(content(0.08))
                .text_color(content(1.))
                .hover(|s| s.bg(content(0.14)))
                .active(|s| s.bg(content(0.2))),
            ButtonLook::Primary => d
                .bg(accent())
                .text_color(on_accent())
                .hover(|s| s.opacity(0.9))
                .active(|s| s.opacity(0.8)),
            // A fixed red: white on the dark theme's `danger` is under 4.5:1.
            ButtonLook::Destructive => d
                .bg(rgb(0xdc2626))
                .text_color(gpui::white())
                .hover(|s| s.opacity(0.9))
                .active(|s| s.opacity(0.8)),
            ButtonLook::Caution => d
                .bg(danger().opacity(0.14))
                .text_color(danger())
                .hover(|s| s.bg(danger().opacity(0.22)))
                .active(|s| s.bg(danger().opacity(0.3))),
            ButtonLook::Ghost => d
                .text_color(content(0.65))
                .hover(|s| s.bg(content(0.08)).text_color(content(1.)))
                .active(|s| s.bg(content(0.14))),
            ButtonLook::Disabled => d.bg(content(0.08)).text_color(content(0.3)),
        })
}

pub fn text_button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    look: ButtonLook,
) -> Stateful<Div> {
    let label = label.into();
    button(id, look).aria_label(label.clone()).child(label)
}

/// An icon button's frame without its icon: a circle `side` across.
pub fn circle_button(id: impl Into<ElementId>, label: &'static str, side: f32) -> Stateful<Div> {
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(side))
        .rounded_full()
}

/// A 24 pt chrome button whose icon brightens on hover.
pub fn icon_button(
    id: &'static str,
    label: &'static str,
    path: &'static str,
    enabled: bool,
) -> Stateful<Div> {
    let ink = content(if enabled { 0.55 } else { 0.3 });
    circle_button(id, label, BUTTON_HEIGHT)
        .group(id)
        .when(enabled, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(content(0.08)))
                .active(|s| s.bg(content(0.14)))
        })
        .child(icon(path, 14.).text_color(ink).when(enabled, |i| {
            i.group_hover(id, |s| s.text_color(content(1.)))
        }))
}

/// A 20 pt button inside a row. `ink` is its icon's color, which a selected
/// row decides.
pub fn row_icon_button(
    id: impl Into<ElementId>,
    label: &'static str,
    path: &'static str,
    ink: Hsla,
) -> Stateful<Div> {
    circle_button(id, label, 20.)
        .cursor_pointer()
        .hover(|s| s.bg(content(0.14)))
        .active(|s| s.bg(content(0.2)))
        .child(icon(path, 12.).text_color(ink))
}

/// A text field's outline: a pill with a 1 pt `border`. The accent when it has
/// the keys, `danger` when what's typed can't be used, else `content(0.12)`.
pub fn field(border: Hsla) -> Div {
    div()
        .rounded_full()
        .border_1()
        .border_color(border)
        .flex()
        .items_center()
}

/// Where a stacked row lives: the row and the gap under it, so a press in the
/// gap still lands. Handlers, the role and the id go here; the highlight stays
/// on the row inside.
pub fn row_slot(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .group(ROW_GROUP)
        .flex_shrink_0()
        .pb(px(ROW_GAP))
        .cursor_pointer()
}

/// A row in a menu, a picker or a popover, inside a [`row_slot`]. `active` is
/// the row the keys or the pointer are on: the accent with its ink.
pub fn row(active: bool) -> Div {
    div()
        .h(px(ROW_HEIGHT))
        .px_2()
        .rounded(px(RADIUS_ROW))
        .flex()
        .items_center()
        .gap_2()
        .text_size(px(TEXT_BODY))
        .map(|d| {
            if active {
                d.bg(accent()).text_color(on_accent())
            } else {
                d.text_color(content(1.))
                    .group_hover(ROW_GROUP, |s| s.bg(content(0.08)))
            }
        })
}

/// Secondary ink in a [`row`]. On the accent it stays opaque: anything less
/// falls under 4.5:1 on some presets.
pub fn row_muted(active: bool) -> Hsla {
    if active { on_accent() } else { content(0.65) }
}

/// A progress bar's 4 pt track. `h_full` fills belong inside it.
pub fn progress_track() -> Div {
    div().h(px(4.)).rounded_full().bg(content(0.14))
}

/// What's filled of a [`progress_track`], from 0 to 1, in the accent.
pub fn progress_fill(fraction: f32) -> Div {
    div()
        .h_full()
        .w(gpui::relative(fraction))
        .rounded_full()
        .bg(accent())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The contrast of `ink`, blended over the opaque `paper`.
    fn contrast(ink: Hsla, paper: Hsla) -> f32 {
        let (ink, paper) = (ink.to_rgb(), paper.to_rgb());
        let luminance = |rgb: [f32; 3]| {
            let [r, g, b] = rgb.map(|v| {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            });
            0.2126 * r + 0.7152 * g + 0.0722 * b
        };
        let blend = |i: f32, p: f32| i * ink.a + p * (1. - ink.a);
        let over = [
            blend(ink.r, paper.r),
            blend(ink.g, paper.g),
            blend(ink.b, paper.b),
        ];
        let (a, b) = (luminance(over), luminance([paper.r, paper.g, paper.b]));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn secondary_text_on_a_selected_row_reads_on_every_preset() {
        for row in palettes::PALETTES {
            for theme in [Theme::Light, Theme::Dark] {
                let p = Preferences {
                    palette: row.0.into(),
                    theme,
                    ..Preferences::default()
                };
                configure(&p, false);
                let ratio = contrast(row_muted(true), accent());
                assert!(ratio >= 4.5, "{}: {ratio}", row.0);
            }
        }
    }
}
