//! The parts an info card is drawn from: the header and the facts under it,
//! which the inspector and column view's last column share.

use crate::theme::{self, content};
use gpui::{AnyElement, Div, FontWeight, div, prelude::*, px};
use monodrive_core::entries::Entry;
use std::collections::BTreeMap;
use std::time::Duration;

/// How long the selection must rest before the disk is read, so arrowing
/// through a list asks nothing about the rows it passes.
pub(super) const READ_DELAY: Duration = Duration::from_millis(100);
/// How much of a value's row its label takes.
pub(super) const LABEL_WIDTH: f32 = 84.;
/// How many kinds the summary of several items names.
pub(super) const KINDS_NAMED: usize = 3;
/// `2 folders, 3 PNG files, 1 more`: the commonest kinds first.
pub(super) fn kinds_summary(entries: &[&Entry]) -> String {
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for entry in entries {
        *kinds.entry(entry.kind_label()).or_default() += 1;
    }
    let mut kinds: Vec<(String, usize)> = kinds.into_iter().collect();
    kinds.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let others: usize = kinds.iter().skip(KINDS_NAMED).map(|(_, count)| count).sum();
    let mut named: Vec<String> = kinds
        .into_iter()
        .take(KINDS_NAMED)
        .map(|(kind, count)| {
            let word = match kind.as_str() {
                "Folder" => "folder",
                "Alias" => "alias",
                "--" => "item",
                other => other,
            };
            let plural = match (count, word) {
                (1, _) => "",
                (_, "alias") => "es",
                _ => "s",
            };
            format!("{count} {word}{plural}")
        })
        .collect();
    if others > 0 {
        named.push(format!("{others} more"));
    }
    named.join(", ")
}
/// The picture, the name and the kind, centered at the top, and the cloud
/// chip after the kind.
pub(super) fn header(
    art: AnyElement,
    name: AnyElement,
    kind: String,
    chip: Option<AnyElement>,
) -> AnyElement {
    div()
        .w_full()
        .pt(px(24.))
        .pb(px(8.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        .child(art)
        .child(
            div()
                .mt(px(8.))
                .max_w_full()
                .text_size(px(theme::TEXT_TITLE))
                .font_weight(FontWeight::MEDIUM)
                .child(name),
        )
        .child(
            div()
                .max_w_full()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_center()
                .gap(px(6.))
                .child(
                    div()
                        .text_center()
                        .text_size(px(theme::TEXT_SMALL))
                        .text_color(content(0.65))
                        .child(kind),
                )
                .children(chip),
        )
        .into_any_element()
}
pub(super) fn fact_row(label: &'static str) -> Div {
    div()
        .min_h(px(24.))
        .flex()
        .flex_shrink_0()
        .items_center()
        .justify_between()
        .gap_2()
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_shrink_0()
                .truncate()
                .text_color(content(0.65))
                .child(label),
        )
}
pub(super) fn fact(label: &'static str, value: String) -> AnyElement {
    fact_ink(label, value, content(1.))
}
pub(super) fn fact_ink(label: &'static str, value: String, ink: gpui::Hsla) -> AnyElement {
    fact_row(label)
        .child(div().min_w_0().text_right().text_color(ink).child(value))
        .into_any_element()
}
