//! What the window's search field needs while a query is typed: where each
//! token sits, so filters and mistakes can be marked where they are, and what
//! could finish the filter at the caret. The text stays the one truth: it's
//! what a kept search stores and what `find_files` takes.

use super::{Kind, Term, spans, term};
use std::ops::Range;

/// A word of typed text and the bytes it takes, quotes included.
#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TokenKind {
    /// For fsearch as typed.
    Word,
    /// A filter, whose key and colon take its first `key` bytes.
    Filter { key: usize },
    /// A filter that doesn't parse, and why.
    Invalid(String),
}

/// The tokens of `text`, in order.
pub fn tokens(text: &str) -> Vec<Token> {
    spans(text)
        .into_iter()
        .map(|(range, word)| {
            let kind = match term(&word) {
                Ok(Term::Word(_)) => TokenKind::Word,
                Ok(_) => TokenKind::Filter {
                    key: text[range.clone()].find(':').map_or(0, |at| at + 1),
                },
                Err(why) => TokenKind::Invalid(why),
            };
            Token { range, kind }
        })
        .collect()
}

/// The filters whose values the field offers to finish.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    Kind,
    Size,
    Modified,
}

const SIZES: [(&str, &str); 4] = [
    (">1mb", "Over 1 MB"),
    (">100mb", "Over 100 MB"),
    (">1gb", "Over 1 GB"),
    ("<100kb", "Under 100 KB"),
];

const DATES: [(&str, &str); 6] = [
    ("today", "Today"),
    ("yesterday", "Yesterday"),
    ("7d", "Last 7 days"),
    ("30d", "Last 30 days"),
    ("1y", "Last year"),
    (">1y", "Over a year ago"),
];

impl Filter {
    pub const ALL: [Filter; 3] = [Filter::Kind, Filter::Size, Filter::Modified];

    pub fn title(self) -> &'static str {
        match self {
            Filter::Kind => "Kind",
            Filter::Size => "Size",
            Filter::Modified => "Date Modified",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Filter::Kind => "kind",
            Filter::Size => "size",
            Filter::Modified => "modified",
        }
    }

    fn of(key: &str) -> Option<Filter> {
        match key {
            "kind" | "type" => Some(Filter::Kind),
            "size" => Some(Filter::Size),
            "modified" | "mtime" => Some(Filter::Modified),
            _ => None,
        }
    }
}

/// A whole token that could take the place of the one at the caret, and
/// what it means.
#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub text: String,
    pub label: &'static str,
}

/// What could finish the token at the caret, and the bytes it takes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Completions {
    pub range: Range<usize>,
    pub items: Vec<Completion>,
}

/// The values that could finish the filter the caret is typing. Only a
/// known key's value is offered, so typing a name never opens a list.
pub fn complete(text: &str, caret: usize) -> Completions {
    if !text.is_char_boundary(caret) {
        return Completions::default();
    }
    let Some((range, _)) = spans(text)
        .into_iter()
        .find(|(range, _)| range.start < caret && caret <= range.end)
    else {
        return Completions::default();
    };
    // Inside quotes it's a phrase, whatever it says.
    let typed = &text[range.start..caret];
    let items = (!typed.contains('"'))
        .then(|| typed.split_once(':'))
        .flatten()
        .and_then(|(key, value)| Some((Filter::of(key)?, value.to_lowercase())))
        .map(|(filter, value)| match filter {
            Filter::Kind => kinds(&value),
            Filter::Size => presets(filter, &SIZES, &value),
            Filter::Modified => presets(filter, &DATES, &value),
        })
        .unwrap_or_default();
    Completions { range, items }
}

/// Kinds that could follow those already listed, which an app, folder, file
/// or link can't.
fn kinds(value: &str) -> Vec<Completion> {
    let (listed, last) = value.rsplit_once(',').unwrap_or(("", value));
    let had: Vec<Kind> = listed.split(',').filter_map(Kind::parse).collect();
    if had.iter().any(|kind| kind.alone()) {
        return Vec::new();
    }
    let lead = if listed.is_empty() {
        String::new()
    } else {
        format!("{listed},")
    };
    Kind::ALL
        .into_iter()
        .filter(|kind| kind.name().starts_with(last) && kind.name() != last)
        .filter(|kind| !had.contains(kind) && (had.is_empty() || !kind.alone()))
        .map(|kind| Completion {
            text: format!("kind:{lead}{}", kind.name()),
            label: kind.label(),
        })
        .collect()
}

fn presets(filter: Filter, presets: &[(&str, &'static str)], value: &str) -> Vec<Completion> {
    presets
        .iter()
        .filter(|(preset, _)| preset.starts_with(value) && *preset != value)
        .map(|&(preset, label)| Completion {
            text: format!("{}:{preset}", filter.key()),
            label,
        })
        .collect()
}

impl Completions {
    /// `text` with item `ix` in place of the token, and the caret after a
    /// space, ready for the next word.
    pub fn accept(&self, text: &str, ix: usize) -> Option<(String, usize)> {
        let item = self.items.get(ix)?;
        let before = text.get(..self.range.start)?;
        let after = text.get(self.range.end..)?.trim_start();
        let caret = before.len() + item.text.len() + 1;
        Some((format!("{before}{} {after}", item.text), caret))
    }
}

/// `text` ready for `filter`'s value to be picked, and the caret where it
/// goes: after a comma on the kinds already there, in place of a size or
/// date already there, or else at the end.
pub fn start_filter(text: &str, filter: Filter) -> (String, usize) {
    let key = format!("{}:", filter.key());
    let found = spans(text).into_iter().find(|(_, word)| {
        word.split_once(':').and_then(|(key, _)| Filter::of(key)) == Some(filter)
    });
    match found {
        Some((range, word)) => {
            let widens = filter == Filter::Kind
                && matches!(
                    term(&word),
                    Ok(Term::Kind(kinds)) if !kinds.iter().any(|kind| kind.alone())
                );
            let (at, put) = if widens {
                (range.end..range.end, ",")
            } else {
                (range, key.as_str())
            };
            let caret = at.start + put.len();
            let mut edited = text.to_string();
            edited.replace_range(at, put);
            (edited, caret)
        }
        None => {
            let base = text.trim_end();
            let space = if base.is_empty() { "" } else { " " };
            let edited = format!("{base}{space}{key}");
            let caret = edited.len();
            (edited, caret)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(text: &str) -> Vec<String> {
        complete(text, text.len())
            .items
            .into_iter()
            .map(|item| item.text)
            .collect()
    }

    #[test]
    fn tokens_mark_filters_by_their_key_and_bad_ones_with_why() {
        let text = "report kind:image \"two words\" size:huge";
        let kinds: Vec<_> = tokens(text)
            .into_iter()
            .map(|token| (&text[token.range], token.kind))
            .collect();
        assert_eq!(
            kinds,
            [
                ("report", TokenKind::Word),
                ("kind:image", TokenKind::Filter { key: 5 }),
                ("\"two words\"", TokenKind::Word),
                (
                    "size:huge",
                    TokenKind::Invalid(
                        "size:huge: give a size such as >10mb, <=1gb or 1mb..10mb".into()
                    )
                ),
            ]
        );
    }

    #[test]
    fn only_a_known_keys_value_is_offered() {
        assert_eq!(offered("invoice"), Vec::<String>::new());
        assert_eq!(offered("ext:"), Vec::<String>::new());
        assert_eq!(offered("re:^a"), Vec::<String>::new());
        assert_eq!(
            offered("size:>1"),
            ["size:>1mb", "size:>100mb", "size:>1gb"]
        );
        assert_eq!(offered("mtime:y"), ["modified:yesterday"]);
        // What's typed in full is done, so ↩ opens the best hit again.
        assert_eq!(offered("modified:today"), Vec::<String>::new());
        // A phrase in quotes is words, whatever it says.
        let text = "contains:\"daily kind:i\"";
        assert_eq!(complete(text, text.len() - 1).items, Vec::new());
        // The caret's own word, not the one after it.
        let text = "kind:v report";
        assert_eq!(complete(text, 6).items[0].text, "kind:video");
        assert_eq!(complete(text, 6).range, 0..6);
    }

    #[test]
    fn kinds_follow_the_ones_listed_and_never_one_that_must_stand_alone() {
        assert_eq!(
            offered("kind:a"),
            ["kind:audio", "kind:archive", "kind:app"]
        );
        assert_eq!(
            offered("type:image,a"),
            ["kind:image,audio", "kind:image,archive"]
        );
        assert_eq!(offered("kind:folder,"), Vec::<String>::new());
        let labels: Vec<_> = complete("kind:i", 6)
            .items
            .iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, ["Images"]);
    }

    #[test]
    fn accepting_replaces_the_token_and_moves_past_it() {
        let text = "invoice kind:d report";
        let list = complete(text, 14);
        assert_eq!(
            list.accept(text, 0),
            Some(("invoice kind:document report".to_string(), 22))
        );
        let text = "invoice size:";
        assert_eq!(
            complete(text, text.len()).accept(text, 2),
            Some(("invoice size:>1gb ".to_string(), 18))
        );
    }

    #[test]
    fn starting_a_filter_widens_kinds_and_replaces_a_size_or_date() {
        assert_eq!(start_filter("", Filter::Kind), ("kind:".into(), 5));
        assert_eq!(
            start_filter("invoice ", Filter::Modified),
            ("invoice modified:".into(), 17)
        );
        assert_eq!(
            start_filter("kind:image invoice", Filter::Kind),
            ("kind:image, invoice".into(), 11)
        );
        assert_eq!(
            start_filter("kind:folder invoice", Filter::Kind),
            ("kind: invoice".into(), 5)
        );
        assert_eq!(
            start_filter("a size:>1gb b", Filter::Size),
            ("a size: b".into(), 7)
        );
        assert_eq!(
            start_filter("a \"size:>1gb\" b", Filter::Size),
            ("a size: b".into(), 7)
        );
    }
}
