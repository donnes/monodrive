//! The AirDrop page: who this Mac sends as, a place to drop files to send,
//! and who can find this Mac.

use crate::browser::pane::Pane;
use crate::browser::pane::drag::{Spot, Target, offers};
use crate::sharing::{self, AirDrop, Discoverable};
use crate::theme::{self, ButtonLook, content, text_button};
use gpui::{AnyElement, Context, FontWeight, PathPromptOptions, Window, div, prelude::*, px};
use std::path::PathBuf;

impl Pane {
    /// Reads who this Mac is and who can find it, which System Settings may
    /// have changed since the page was last open.
    pub(in crate::browser) fn read_airdrop(&mut self, cx: &mut Context<Self>) {
        if cx.try_global::<AirDrop>().is_none_or(|shown| shown.live) {
            cx.set_global(AirDrop::read());
        }
    }

    /// Sends `paths` through AirDrop's window, or says why it can't.
    pub(in crate::browser) fn airdrop(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if let Err(why) = sharing::send(paths) {
            self.toast(why, true, None, cx);
        }
    }

    fn choose_and_airdrop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Send".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| this.airdrop(&paths, cx));
        })
        .detach();
    }

    pub(in crate::browser) fn airdrop_page(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.showing_airdrop() {
            return None;
        }
        let shown = cx.try_global::<AirDrop>()?.clone();
        Some(
            div()
                .id("airdrop-page")
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px(px(theme::PANE_INSET))
                        .py_4()
                        .border_b_1()
                        .border_color(content(0.06))
                        .child(avatar(&shown.me))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(theme::TEXT_SMALL))
                                        .text_color(content(0.65))
                                        .child("Use AirDrop as"),
                                )
                                .child(
                                    div()
                                        .text_size(px(theme::TEXT_TITLE))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(shown.me.clone()),
                                ),
                        ),
                )
                .child(self.drop_zone(cx))
                .child(discovery(shown.discoverable))
                .into_any_element(),
        )
    }

    /// Files dropped here or chosen go to AirDrop's own window, which asks
    /// who gets them: only Apple's own apps can list the people nearby.
    fn drop_zone(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let drag = self.drag.clone();
        let spot = Spot::Pane(cx.entity_id());
        let outlined = self.drag.outlines(&spot);
        div()
            .id("airdrop-drop")
            .flex_1()
            .m(px(theme::PANE_INSET))
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(if outlined {
                theme::accent()
            } else {
                content(0.14)
            })
            .when(outlined, |d| d.bg(theme::accent().opacity(0.08)))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_6()
            .when(self.drag.active(), |d| {
                offers(d, move |_| drag.offer(Target::AirDrop(spot)))
            })
            .child(
                div()
                    .text_size(px(theme::TEXT_TITLE))
                    .font_weight(FontWeight::MEDIUM)
                    .child("Drop files here to send them with AirDrop"),
            )
            .child(
                div()
                    .text_size(px(theme::TEXT_SMALL))
                    .text_color(content(0.65))
                    .child("AirDrop then asks who to send them to."),
            )
            .child(div().pt_2().child(
                text_button("airdrop-choose", "Choose Files…", ButtonLook::Secondary).on_click(
                    cx.listener(|this, _, window, cx| this.choose_and_airdrop(window, cx)),
                ),
            ))
    }
}

/// Who can find this Mac, which only System Settings and Control Center change.
fn discovery(mode: Option<Discoverable>) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .px(px(theme::PANE_INSET))
        .py_3()
        .border_t_1()
        .border_color(content(0.06))
        .text_size(px(theme::TEXT_SMALL))
        .child(
            div()
                .text_color(content(0.65))
                .child("AirDrop lets you share instantly with people nearby."),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_color(content(0.65))
                        .child("Allow me to be discovered by"),
                )
                .child(
                    div()
                        .text_color(content(1.))
                        .child(mode.map_or("Unknown", Discoverable::label)),
                )
                .child(
                    text_button("airdrop-settings", "Change…", ButtonLook::Ghost)
                        .on_click(|_, _, _| sharing::open_settings()),
                ),
        )
}

/// A circle with this Mac's user's initials, as AirDrop shows them.
fn avatar(name: &str) -> impl IntoElement + use<> {
    let initials: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    div()
        .size(px(48.))
        .flex_shrink_0()
        .rounded_full()
        .bg(theme::accent())
        .flex()
        .items_center()
        .justify_center()
        .text_color(theme::on_accent())
        .text_size(px(theme::TEXT_TITLE))
        .font_weight(FontWeight::MEDIUM)
        .child(initials)
}
