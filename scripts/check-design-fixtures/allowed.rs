// What the check must leave alone.
use gpui::{div, prelude::*, px};

fn on_the_scale() {
    div()
        .rounded(px(theme::RADIUS_CARD - 1.))
        .rounded(
            px(RADIUS_ROW),
        )
        .rounded_full()
        .text_size(
            px(theme::TEXT_BODY),
        )
        .text_size(px(label))
        .bg(content(0.08))
        .text_color(content(if on { 1. } else { 0.65 }))
        .child(icon(if open { "a" } else { "b" }, 12.));
    accent().to_rgb();
}

fn not_code() {
    let _ = "smb://nas.local .rounded(px(5.)) rgb(0x1)";
    // .rounded(px(5.)) and rgb(0x1) in a comment.
}

fn excused() {
    // design: a swatch shows the color it offers.
    div().bg(gpui::rgb(0x1));
    div().bg(gpui::rgb(0x2)); // design: so does this one.
}
