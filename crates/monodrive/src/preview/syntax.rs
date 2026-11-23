//! Code split into the kinds Monocode's editor colors, which the palette turns
//! into colors as it draws, so a theme or palette change needs no new read.

use crate::palettes::Syntax;
use gpui::SharedString;
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;
use syntect::easy::ScopeRangeIterator;
use syntect::highlighting::ScopeSelectors;
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

/// Beyond this, text stays plain: coloring walks the whole of it.
const LIMIT: usize = 512 * 1024;

pub type Spans = Vec<(Range<usize>, Syntax)>;

/// A line without its ending, and what its code is.
#[derive(Clone)]
pub struct Line {
    pub text: SharedString,
    pub spans: Spans,
}

/// TextMate scopes and what code under them is, as Dark+ groups them; `None`
/// keeps the ink. The most specific match wins, as in a theme.
const KINDS: &[(&str, Option<Syntax>)] = &[
    ("comment", Some(Syntax::Comment)),
    (
        "string, markup.raw, support.constant.property-value",
        Some(Syntax::String),
    ),
    ("constant.numeric, keyword.other.unit", Some(Syntax::Number)),
    (
        "constant.language, variable.language",
        Some(Syntax::Keyword),
    ),
    ("keyword, storage", Some(Syntax::Keyword)),
    ("keyword.operator", None),
    (
        "entity.name.function, support.function, variable.function",
        Some(Syntax::Callable),
    ),
    (
        "entity.name.type, entity.name.class, entity.name.struct, entity.name.enum, \
         entity.name.trait, entity.name.interface, entity.name.namespace, \
         entity.name.table, entity.other.inherited-class, support.type, support.class",
        Some(Syntax::Type),
    ),
    ("entity.name.tag", Some(Syntax::Tag)),
    (
        "entity.other.attribute-name, variable.other.member, variable.other.property, \
         support.type.property-name, meta.object-literal.key, meta.mapping.key string",
        Some(Syntax::Property),
    ),
    (
        "meta.annotation, meta.attribute, meta.decorator, meta.preprocessor",
        Some(Syntax::Meta),
    ),
    ("invalid", Some(Syntax::Invalid)),
    ("markup.heading", Some(Syntax::Heading)),
];

fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(two_face::syntax::extra_newlines)
}

fn kinds() -> &'static [(ScopeSelectors, Option<Syntax>)] {
    static KINDS_PARSED: OnceLock<Vec<(ScopeSelectors, Option<Syntax>)>> = OnceLock::new();
    KINDS_PARSED.get_or_init(|| {
        KINDS
            .iter()
            .map(|(selector, kind)| (selector.parse().expect("a valid selector"), *kind))
            .collect()
    })
}

/// Names that mean a syntax found under another name.
fn alias(name: &str) -> &str {
    match name {
        // fancy-regex can't run bat's JSX grammar; its TSX one reads JSX too.
        "jsx" => "tsx",
        "mjs" | "cjs" => "js",
        "jsonc" => "json",
        "shell" | "console" | "zsh" => "sh",
        other => other,
    }
}

/// A file's lines, colored when its name, extension or first line says what
/// it is.
pub fn file(text: &str, path: &Path) -> Vec<Line> {
    let set = syntaxes();
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let syntax = set
        .find_syntax_by_extension(&name)
        .or_else(|| set.find_syntax_by_extension(alias(&extension)))
        .or_else(|| set.find_syntax_by_first_line(text));
    let mut lines = Vec::new();
    each_line(text, syntax, |_, line, spans| {
        lines.push(Line {
            text: SharedString::from(line.to_string()),
            spans,
        })
    });
    lines
}

/// A Markdown fence's colors, over the whole of `text`. `info` is what follows
/// the opening backticks, such as `rust` or `ts title="a.ts"`.
pub fn fence(text: &str, info: &str) -> Spans {
    let tag = info
        .split([' ', ',', '{'])
        .next()
        .unwrap_or_default()
        .to_lowercase();
    let syntax = (!tag.is_empty())
        .then(|| syntaxes().find_syntax_by_token(alias(&tag)))
        .flatten();
    let mut all = Spans::new();
    each_line(text, syntax, |start, _, spans| {
        all.extend(
            spans
                .into_iter()
                .map(|(range, kind)| (start + range.start..start + range.end, kind)),
        )
    });
    all
}

/// Each line of `text` with its start, its text without the line ending, and
/// its colors when `syntax` is known and the text isn't too long to color.
fn each_line(
    text: &str,
    syntax: Option<&SyntaxReference>,
    mut each: impl FnMut(usize, &str, Spans),
) {
    let set = syntaxes();
    let mut parser = syntax
        .filter(|_| text.len() <= LIMIT)
        .map(|syntax| (ParseState::new(syntax), ScopeStack::new()));
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        let stripped = line.trim_end_matches(['\n', '\r']);
        let spans = match &mut parser {
            Some((state, stack)) => colored(line, stripped.len(), state, stack, set),
            None => Spans::new(),
        };
        each(start, stripped, spans);
        start += line.len();
    }
}

/// One line's colors, up to `end`. The grammars expect each line to end in a
/// newline, the last one too.
fn colored(
    line: &str,
    end: usize,
    state: &mut ParseState,
    stack: &mut ScopeStack,
    set: &SyntaxSet,
) -> Spans {
    let owned;
    let line = if line.ends_with('\n') {
        line
    } else {
        owned = format!("{line}\n");
        &owned
    };
    let Ok(ops) = state.parse_line(line, set) else {
        return Spans::new();
    };
    let mut spans = Spans::new();
    for (range, op) in ScopeRangeIterator::new(&ops, line) {
        if stack.apply(op).is_err() {
            break;
        }
        let range = range.start..range.end.min(end);
        if range.is_empty() {
            continue;
        }
        let Some(kind) = kind_of(stack) else {
            continue;
        };
        match spans.last_mut() {
            Some((last, last_kind)) if *last_kind == kind && last.end == range.start => {
                last.end = range.end
            }
            _ => spans.push((range, kind)),
        }
    }
    spans
}

fn kind_of(stack: &ScopeStack) -> Option<Syntax> {
    let scopes = stack.as_slice();
    kinds()
        .iter()
        .filter_map(|(selector, kind)| Some((selector.does_match(scopes)?, *kind)))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .and_then(|(_, kind)| kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds_in(text: &str, name: &str) -> Spans {
        file(text, Path::new(name)).remove(0).spans
    }

    #[test]
    fn colors_cover_the_line_without_its_ending() {
        let lines = file("fn a() {}\r\nlet b = 1;", Path::new("a.rs"));
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text.as_ref(), "fn a() {}");
        assert_eq!(lines[0].spans[0], (0..2, Syntax::Keyword));
        assert!(lines[0].spans.iter().all(|(range, _)| range.end <= 9));
        assert_eq!(
            lines[1].spans[0],
            (0..3, Syntax::Keyword),
            "the last line too"
        );
    }

    #[test]
    fn a_syntax_comes_from_the_name_the_extension_or_the_first_line() {
        assert_eq!(
            kinds_in("FROM alpine", "Dockerfile")[0],
            (0..4, Syntax::Keyword)
        );
        assert_eq!(kinds_in("const a = 1", "a.tsx")[0], (0..5, Syntax::Keyword));
        assert_eq!(kinds_in("// a", "a.jsonc")[0], (0..4, Syntax::Comment));
        assert_eq!(kinds_in("#!/bin/sh\n", "run")[0].1, Syntax::Comment);
        assert!(kinds_in("const a = 1", "a.unknownext").is_empty());
    }
}
