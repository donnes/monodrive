// Forbidden values in ordinary formatting. The tilde comments mark each line
// the check must report; a call spanning lines is reported where it starts.
use gpui::rgb as x; //~
use gpui::{div, hsla as y, prelude::*, px}; //~

fn continued_calls() {
    div()
        .text_size( //~
            px(19.),
        )
        .rounded( //~
            px(5.),
        )
        .text_size(px(12.5)); //~
}

fn aliased_colors() {
    div().bg(gpui::rgba(0xff0000ff)); //~
    let paint = gpui::black; //~
    let _ = (paint, x(0xff0000), y(0., 0., 0., 1.));
}

fn every_alpha_on_a_line() {
    div().bg(content(0.08)).text_color(content(0.4)); //~
    div().text_color(content(if on { 1. } else { 0.35 })); //~
    div().bg(content( //~
        0.5,
    ));
}

fn sizes() {
    div().rounded_md(); //~
    div().child(icon(page.icon(), 15.)); //~
    div().shadow(vec![]); //~
}
