//! Monocode's appearance tokens: a glass shell with content ink drawn at
//! varying alpha instead of fixed greys. [`configure`] sets them from the
//! preferences before each frame; views read them through these functions.

use crate::palettes;
use crate::preferences::{Preferences, Theme};
use gpui::{
    Div, FontWeight, Hsla, Role, Stateful, Svg, WindowBackgroundAppearance, canvas, div,
    prelude::*, px, rgb, svg,
};
use std::cell::Cell;

pub const UI_FONT: &str = ".SystemUIFont";
pub const TITLE_BAR_HEIGHT: f32 = 40.;
pub const ROW_HEIGHT: f32 = 28.;

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

pub fn black(alpha: f32) -> Hsla {
    gpui::hsla(0., 0., 0., alpha)
}

/// A frosted fill for floating surfaces such as the settings sheet and menus,
/// or an opaque one in light themes. Place it first in a `relative()` parent
/// with the same corners.
pub fn floating_surface(radius: f32) -> impl IntoElement {
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
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(content(0.5))
        .child(label)
}

pub fn icon(path: &'static str, side: f32) -> Svg {
    svg()
        .path(path)
        .size(px(side))
        .text_color(content(0.5))
        .flex_shrink_0()
}

/// A 24 pt chrome button whose icon brightens on hover.
pub fn icon_button(
    id: &'static str,
    label: &'static str,
    path: &'static str,
    enabled: bool,
) -> Stateful<Div> {
    let ink = content(if enabled { 0.5 } else { 0.25 });
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label)
        .group(id)
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded_md()
        .when(enabled, |d| {
            d.cursor_pointer().hover(|s| s.bg(content(0.1)))
        })
        .child(icon(path, 14.).text_color(ink).when(enabled, |i| {
            i.group_hover(id, |s| s.text_color(content(1.)))
        }))
}
