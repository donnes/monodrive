//! Text selected in a preview that draws its text as rows: a file's lines, or
//! Markdown's blocks.

use gpui::{HighlightStyle, Hsla};
use std::ops::Range;

/// A place between two characters: its row, and a byte offset in that row.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Caret {
    pub row: usize,
    pub offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    anchor: Caret,
    pub head: Caret,
}

impl Selection {
    pub fn between(anchor: Caret, head: Caret) -> Self {
        Self { anchor, head }
    }

    pub fn at(caret: Caret) -> Self {
        Self::between(caret, caret)
    }

    fn ends(&self) -> (Caret, Caret) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    /// What's selected of `row`, which is `len` bytes long.
    pub fn in_row(&self, row: usize, len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ends();
        if self.is_empty() || row < start.row || row > end.row {
            return None;
        }
        let from = if row == start.row { start.offset } else { 0 };
        let to = if row == end.row { end.offset } else { len };
        Some(from.min(len)..to.min(len))
    }

    /// The selected text, a line for each row it covers.
    pub fn text<'a>(&self, row: impl Fn(usize) -> &'a str) -> String {
        let (start, end) = self.ends();
        (start.row..=end.row)
            .filter_map(|ix| {
                let text = row(ix);
                let range = self.in_row(ix, text.len())?;
                text.get(range)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The word around `offset`, or the run of spaces or the one mark there.
pub fn word(text: &str, offset: usize) -> Range<usize> {
    fn class(c: char) -> u8 {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    }
    let offset = offset.min(text.len());
    let Some(at) = text[offset..]
        .chars()
        .next()
        .or_else(|| text[..offset].chars().next_back())
    else {
        return offset..offset;
    };
    let kind = class(at);
    if kind == 2 {
        let start = if text[offset..].starts_with(at) {
            offset
        } else {
            offset - at.len_utf8()
        };
        return start..start + at.len_utf8();
    }
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|(_, c)| class(*c) == kind)
        .last()
        .map_or(offset, |(ix, _)| ix);
    let end = text[offset..]
        .char_indices()
        .find(|(_, c)| class(*c) != kind)
        .map_or(text.len(), |(ix, _)| offset + ix);
    start..end
}

/// `styles`, sorted and apart, with `selected` painted `background` over them.
pub fn highlighted(
    styles: Vec<(Range<usize>, HighlightStyle)>,
    selected: Option<Range<usize>>,
    background: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let Some(selected) = selected.filter(|range| !range.is_empty()) else {
        return styles;
    };
    // Every byte up to the selection's end, styled or not, as runs.
    let mut runs = Vec::new();
    let mut at = 0;
    for (range, style) in styles {
        if at < range.start {
            runs.push((at..range.start, HighlightStyle::default()));
        }
        at = range.end;
        runs.push((range, style));
    }
    if at < selected.end {
        runs.push((at..selected.end, HighlightStyle::default()));
    }
    let mut out = Vec::new();
    for (range, style) in runs {
        let cuts = [range.start, selected.start, selected.end, range.end];
        for pair in cuts.windows(2) {
            let part = pair[0].max(range.start)..pair[1].min(range.end);
            if part.is_empty() {
                continue;
            }
            let inside = part.start >= selected.start && part.end <= selected.end;
            let style = if inside {
                HighlightStyle {
                    background_color: Some(background),
                    ..style
                }
            } else {
                style
            };
            out.push((part, style));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caret(row: usize, offset: usize) -> Caret {
        Caret { row, offset }
    }

    #[test]
    fn a_selection_copies_its_rows_in_order_whichever_way_it_was_dragged() {
        let rows = ["one", "two", "three"];
        let backwards = Selection::between(caret(2, 2), caret(0, 1));
        assert_eq!(backwards.text(|ix| rows[ix]), "ne\ntwo\nth");
        assert_eq!(backwards.in_row(1, 3), Some(0..3));
        assert_eq!(backwards.in_row(3, 3), None);
        assert_eq!(Selection::at(caret(1, 1)).text(|ix| rows[ix]), "");
    }

    #[test]
    fn a_word_is_letters_spaces_or_one_mark() {
        let text = "let café = a.b;";
        assert_eq!(&text[word(text, 5)], "café");
        assert_eq!(&text[word(text, 3)], " ");
        assert_eq!(&text[word(text, 13)], ".");
        // At the end, the word before.
        assert_eq!(&text[word(text, text.len())], ";");
        assert_eq!(word("", 0), 0..0);
    }

    #[test]
    fn the_selection_paints_over_colors_and_the_gaps_between() {
        let red = HighlightStyle {
            font_weight: Some(gpui::FontWeight::BOLD),
            ..Default::default()
        };
        let bg = Hsla::default();
        let out = highlighted(vec![(2..6, red)], Some(4..9), bg);
        let ranges: Vec<_> = out.iter().map(|(range, _)| range.clone()).collect();
        assert_eq!(ranges, [0..2, 2..4, 4..6, 6..9]);
        assert_eq!(out[1].1, red);
        assert_eq!(out[2].1.background_color, Some(bg));
        assert_eq!(out[2].1.font_weight, red.font_weight);
        assert_eq!(out[3].1.background_color, Some(bg));
        assert_eq!(highlighted(vec![(2..6, red)], None, bg), [(2..6, red)]);
    }
}
