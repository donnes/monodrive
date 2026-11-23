//! What the window's search field needs while a query is typed: where each
//! token sits, so a mistake can be marked where it is, what could finish the
//! filter at the caret, and the finished filters lifted out of the text into
//! tokens, each with words a person reads. Tokens and words together are one
//! `Query`, whose text is what a kept search stores and what `find_files`
//! takes.

use super::{Age, AgeUnit, Amount, Bound, Day, Kind, Modified, Query, Term, Unit, spans, term};
use std::mem::discriminant;
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

    /// The filter `term` is, if the field offers values for it.
    pub fn of_term(term: &Term) -> Option<Filter> {
        match term {
            Term::Kind(_) => Some(Filter::Kind),
            Term::Size(_) => Some(Filter::Size),
            Term::Modified(_) => Some(Filter::Modified),
            _ => None,
        }
    }

    /// A size's or date's values to pick from, each with what it means.
    /// Kinds are picked one by one with `toggle_kind` instead.
    pub fn options(self) -> Vec<(Term, &'static str)> {
        let presets: &[(&str, &'static str)] = match self {
            Filter::Kind => &[],
            Filter::Size => &SIZES,
            Filter::Modified => &DATES,
        };
        presets
            .iter()
            .filter_map(|&(value, label)| {
                Some((term(&format!("{}:{value}", self.key())).ok()?, label))
            })
            .collect()
    }
}

/// `kinds` with `kind` turned on or off. An app, folder, file or link stands
/// alone, so turning one on turns the rest off, and the reverse.
pub fn toggle_kind(kinds: &[Kind], kind: Kind) -> Vec<Kind> {
    if kinds.contains(&kind) {
        return kinds.iter().copied().filter(|k| *k != kind).collect();
    }
    if kind.alone() {
        return vec![kind];
    }
    let mut kinds: Vec<Kind> = kinds.iter().copied().filter(|k| !k.alone()).collect();
    kinds.push(kind);
    kinds
}

impl Query {
    /// The query with `term` added the way the window's tokens hold it: a
    /// kind or extension widens the one there, and any other filter takes the
    /// place of its own.
    pub fn with(mut self, term: Term) -> Query {
        let same = match &term {
            Term::Word(_) => None,
            _ => self
                .terms
                .iter_mut()
                .find(|had| discriminant(*had) == discriminant(&term)),
        };
        match (same, term) {
            (Some(Term::Kind(had)), Term::Kind(more)) => {
                for kind in more {
                    if !had.contains(&kind) {
                        had.push(kind);
                    }
                }
            }
            (Some(Term::Ext(had)), Term::Ext(more)) => {
                for ext in more {
                    if !had.contains(&ext) {
                        had.push(ext);
                    }
                }
            }
            (Some(slot), term) => *slot = term,
            (None, term) => self.terms.push(term),
        }
        self
    }
}

/// What's typed with its finished filters lifted into tokens.
#[derive(Clone, Debug, PartialEq)]
pub struct Lifted {
    pub filters: Query,
    pub text: String,
    pub caret: usize,
}

/// Takes the finished filters out of `text` into `filters`: every one a
/// space follows, but what the caret is in or ends, while adding it leaves a
/// query fsearch can answer. Each goes with a space beside it. `None` when
/// none is finished.
pub fn lift(filters: &Query, text: &str, caret: usize) -> Option<Lifted> {
    lift_where(filters, text, caret, |range| {
        (range.start <= caret && caret <= range.end) || !text[range.end..].starts_with(' ')
    })
}

/// A kept search's text as the window shows it: its filters as tokens, and
/// the words left to type.
pub fn split(text: &str) -> (Query, String) {
    match lift_where(&Query::default(), text, usize::MAX, |_| false) {
        Some(lifted) => (lifted.filters, lifted.text.trim().to_string()),
        None => (Query::default(), text.trim().to_string()),
    }
}

fn lift_where(
    filters: &Query,
    text: &str,
    caret: usize,
    stays: impl Fn(&Range<usize>) -> bool,
) -> Option<Lifted> {
    let mut query = filters.clone();
    let mut cut = Vec::new();
    for (range, word) in spans(text) {
        if stays(&range) {
            continue;
        }
        let Ok(found) = term(&word) else { continue };
        if matches!(found, Term::Word(_)) {
            continue;
        }
        let widened = query.clone().with(found);
        if widened.check().is_ok() {
            query = widened;
            cut.push(range);
        }
    }
    if cut.is_empty() {
        return None;
    }
    let (mut kept, mut moved, mut at) = (String::new(), caret, 0);
    for range in cut {
        let (start, end) = if text[range.end..].starts_with(' ') {
            (range.start, range.end + 1)
        } else if text[..range.start].ends_with(' ') {
            ((range.start - 1).max(at), range.end)
        } else {
            (range.start, range.end)
        };
        kept.push_str(&text[at..start]);
        if start < caret {
            moved -= end.min(caret) - start;
        }
        at = end;
    }
    kept.push_str(&text[at..]);
    Some(Lifted {
        filters: query,
        text: kept,
        caret: moved,
    })
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

impl Term {
    /// What it means in a few words, as a token shows it.
    pub fn label(&self) -> String {
        match self {
            Term::Word(word) => word.clone(),
            Term::Kind(kinds) => kinds
                .iter()
                .map(|kind| kind.label())
                .collect::<Vec<_>>()
                .join(", "),
            Term::Ext(exts) => exts
                .iter()
                .map(|ext| format!(".{ext}"))
                .collect::<Vec<_>>()
                .join(", "),
            Term::Size(size) => bound(size, amount, ["Over", "or more", "Under", "or less"]),
            Term::Modified(Modified::Within(age)) if age.n == 1 => format!("Last {}", unit(age)),
            Term::Modified(Modified::Within(age)) => format!("Last {} {}s", age.n, unit(age)),
            Term::Modified(Modified::OlderThan(age)) if age.n == 1 => {
                format!(
                    "Over a{} {} ago",
                    if age.unit == AgeUnit::Hours { "n" } else { "" },
                    unit(age)
                )
            }
            Term::Modified(Modified::OlderThan(age)) => {
                format!("Over {} {}s ago", age.n, unit(age))
            }
            Term::Modified(Modified::Days(days)) => {
                let text = bound(days, day, ["After", "or later", "Before", "or earlier"]);
                let mut chars = text.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            }
            Term::In(path) => format!(
                "In {}",
                path.file_name()
                    .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
            ),
            Term::Contains(text) => format!("Contains \u{201c}{text}\u{201d}"),
        }
    }
}

fn bound<T>(
    bound: &Bound<T>,
    show: fn(&T) -> String,
    [over, at_least, under, at_most]: [&str; 4],
) -> String {
    match bound {
        Bound::Over(x) => format!("{over} {}", show(x)),
        Bound::AtLeast(x) => format!("{} {at_least}", show(x)),
        Bound::Under(x) => format!("{under} {}", show(x)),
        Bound::AtMost(x) => format!("{} {at_most}", show(x)),
        Bound::Between(a, b) => format!("{} to {}", show(a), show(b)),
        Bound::Is(x) => show(x),
    }
}

fn amount(amount: &Amount) -> String {
    let unit = match amount.unit {
        Unit::B => "bytes",
        Unit::Kb => "KB",
        Unit::Mb => "MB",
        Unit::Gb => "GB",
        Unit::Tb => "TB",
    };
    format!("{} {unit}", amount.n)
}

fn unit(age: &Age) -> &'static str {
    match age.unit {
        AgeUnit::Hours => "hour",
        AgeUnit::Days => "day",
        AgeUnit::Weeks => "week",
        AgeUnit::Months => "month",
        AgeUnit::Years => "year",
    }
}

fn day(day: &Day) -> String {
    match day {
        Day::Today => "today".into(),
        Day::Yesterday => "yesterday".into(),
        Day::Date { year, month, day } => {
            let month = MONTHS.get(usize::from(*month) - 1).copied().unwrap_or("");
            format!("{month} {day}, {year}")
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

/// The values that could finish the filter the caret is typing, beside the
/// `filters` already tokens. Only a known key's value is offered, so typing
/// a name never opens a list.
pub fn complete(filters: &Query, text: &str, caret: usize) -> Completions {
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
            Filter::Kind => kinds(filters, &value),
            Filter::Size => presets(filter, &SIZES, &value),
            Filter::Modified => presets(filter, &DATES, &value),
        })
        .unwrap_or_default();
    Completions { range, items }
}

/// Kinds that could join those listed and those the tokens have, which an
/// app, folder, file or link can't.
fn kinds(filters: &Query, value: &str) -> Vec<Completion> {
    let (listed, last) = value.rsplit_once(',').unwrap_or(("", value));
    let tokens = filters.terms.iter().flat_map(|term| match term {
        Term::Kind(kinds) => kinds.as_slice(),
        _ => &[],
    });
    let had: Vec<Kind> = listed
        .split(',')
        .filter_map(Kind::parse)
        .chain(tokens.copied())
        .collect();
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
            let put = if widens { format!("{word},") } else { key };
            let caret = range.start + put.len();
            let mut edited = text.to_string();
            edited.replace_range(range, &put);
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
        complete(&Query::default(), text, text.len())
            .items
            .into_iter()
            .map(|item| item.text)
            .collect()
    }

    fn labels(text: &str) -> Vec<String> {
        Query::parse(text)
            .unwrap()
            .terms
            .iter()
            .map(Term::label)
            .collect()
    }

    fn lifted(text: &str, caret: usize) -> Option<(String, String, usize)> {
        lift(&Query::default(), text, caret)
            .map(|lifted| (lifted.filters.to_string(), lifted.text, lifted.caret))
    }

    #[test]
    fn finished_filters_lift_out_and_the_one_being_typed_stays() {
        // The caret ends `kind:image`, so it's still being typed.
        assert_eq!(lifted("report kind:image", 17), None);
        assert_eq!(
            lifted("report kind:image ", 18),
            Some(("kind:image".into(), "report ".into(), 7))
        );
        // Before the caret and after it, each with a space beside it, but
        // only once a space has followed it.
        assert_eq!(
            lifted("size:>1gb a b modified:7d c", 11),
            Some(("size:>1gb modified:7d".into(), "a b c".into(), 1))
        );
        assert_eq!(lifted("notes kind:doc", 0), None);
        // A bad filter and one fsearch can't add stay words.
        assert_eq!(
            lift(
                &Query::parse("kind:image size:>1gb").unwrap(),
                "kind:folder size:huge x",
                usize::MAX
            ),
            None
        );
        // Kinds widen the token there, and a size replaces it.
        let had = Query::parse("kind:image size:>1gb").unwrap();
        let after = lift(&had, "kind:video size:<1mb ", 21).unwrap();
        assert_eq!(after.filters.to_string(), "kind:image,video size:<1mb");
        assert_eq!(after.text, "");
        assert_eq!(
            split("notes kind:doc \"two words\""),
            (
                Query::parse("kind:document").unwrap(),
                "notes \"two words\"".into()
            )
        );
    }

    #[test]
    fn tokens_read_as_words() {
        let one = |text: &str| labels(text).remove(0);
        assert_eq!(one("kind:image,video"), "Images, Videos");
        assert_eq!(one("ext:rs,toml"), ".rs, .toml");
        assert_eq!(one("size:>10mb"), "Over 10 MB");
        assert_eq!(one("size:<=100"), "100 bytes or less");
        assert_eq!(one("size:1.5kb..2gb"), "1.5 KB to 2 GB");
        assert_eq!(one("modified:1d"), "Last day");
        assert_eq!(one("modified:30d"), "Last 30 days");
        assert_eq!(one("modified:>1h"), "Over an hour ago");
        assert_eq!(one("modified:>2y"), "Over 2 years ago");
        assert_eq!(one("modified:today"), "Today");
        assert_eq!(one("modified:>=2026-01-01"), "Jan 1, 2026 or later");
        assert_eq!(one("modified:<yesterday"), "Before yesterday");
        assert_eq!(one("in:~/Work"), "In Work");
        assert_eq!(
            one("contains:\"fix me\""),
            "Contains \u{201c}fix me\u{201d}"
        );
    }

    #[test]
    fn a_standalone_kind_replaces_the_rest_and_the_rest_replace_it() {
        use Kind::*;
        assert_eq!(toggle_kind(&[Image], Video), [Image, Video]);
        assert_eq!(toggle_kind(&[Image, Video], Image), [Video]);
        assert_eq!(toggle_kind(&[Image, Video], Folder), [Folder]);
        assert_eq!(toggle_kind(&[Folder], Image), [Image]);
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
        assert_eq!(
            complete(&Query::default(), text, text.len() - 1).items,
            Vec::new()
        );
        // The caret's own word, not the one after it.
        let text = "kind:v report";
        assert_eq!(
            complete(&Query::default(), text, 6).items[0].text,
            "kind:video"
        );
        assert_eq!(complete(&Query::default(), text, 6).range, 0..6);
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
        // The kinds the tokens have count too.
        let had = |tokens: &str, text: &str| -> Vec<String> {
            let tokens = Query::parse(tokens).unwrap();
            let list = complete(&tokens, text, text.len());
            list.items.into_iter().map(|item| item.text).collect()
        };
        assert_eq!(had("kind:image", "kind:i"), Vec::<String>::new());
        assert_eq!(had("kind:image", "kind:a"), ["kind:audio", "kind:archive"]);
        assert_eq!(had("kind:folder", "kind:"), Vec::<String>::new());
        let labels: Vec<_> = complete(&Query::default(), "kind:i", 6)
            .items
            .iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, ["Images"]);
    }

    #[test]
    fn accepting_replaces_the_token_and_moves_past_it() {
        let text = "invoice kind:d report";
        let list = complete(&Query::default(), text, 14);
        assert_eq!(
            list.accept(text, 0),
            Some(("invoice kind:document report".to_string(), 22))
        );
        let text = "invoice size:";
        assert_eq!(
            complete(&Query::default(), text, text.len()).accept(text, 2),
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
            start_filter("kind:\"image\"", Filter::Kind),
            ("kind:image,".into(), 11)
        );
        assert_eq!(
            start_filter("a \"size:>1gb\" b", Filter::Size),
            ("a size: b".into(), 7)
        );
    }
}
