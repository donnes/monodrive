//! What a preview shows for a path, and the bounded read behind text. Deciding
//! never opens the file, so a placeholder or a gated file can't pick a renderer
//! that reads it.

use crate::cloud::{self, Availability};
use crate::sizes::needs_consent;
use std::io::Read;
use std::ops::Range;
use std::path::Path;

/// How much of a text file a preview reads.
pub const TEXT_LIMIT: u64 = 5 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Decoded through the system's generator, at the size drawn.
    Picture,
    /// Plays by itself.
    Animated,
    Video,
    Audio,
    Markdown,
    Html,
    Text,
    /// Text whose values are secrets: `.env`, `.env.local`, `prod.env`.
    Env,
    Zip,
    /// Contents that aren't on this Mac. Reading would download them.
    NotDownloaded,
    /// Reading could show a privacy prompt.
    NeedsAccess,
    /// A format Apple's media frameworks can't open.
    Unplayable,
    /// Apple's Quick Look draws it: PDF, Office, iWork, fonts, plugins, the rest.
    System,
}

fn by_extension(extension: &str) -> Kind {
    match extension {
        "gif" => Kind::Animated,
        "png" | "jpg" | "jpeg" | "webp" | "bmp" | "tif" | "tiff" | "heic" | "heif" | "dng"
        | "cr2" | "cr3" | "nef" | "arw" | "raf" | "orf" | "rw2" => Kind::Picture,
        "mov" | "mp4" | "m4v" | "mpg" | "3gp" => Kind::Video,
        "mkv" | "webm" | "avi" => Kind::Unplayable,
        "mp3" | "m4a" | "aac" | "wav" | "aiff" | "aif" | "flac" | "caf" => Kind::Audio,
        "md" | "markdown" | "mdown" => Kind::Markdown,
        "html" | "htm" | "xhtml" => Kind::Html,
        "zip" => Kind::Zip,
        "txt" | "text" | "log" | "csv" | "tsv" | "json" | "jsonc" | "toml" | "yaml" | "yml"
        | "xml" | "ini" | "cfg" | "conf" | "env" | "rs" | "py" | "js" | "jsx" | "mjs" | "ts"
        | "tsx" | "go" | "c" | "h" | "cc" | "cpp" | "hpp" | "m" | "mm" | "swift" | "java"
        | "kt" | "rb" | "php" | "sh" | "bash" | "zsh" | "fish" | "css" | "scss" | "sql" | "lua"
        | "pl" | "r" | "vue" | "svelte" | "gitignore" | "gitattributes" | "gitmodules" | "lock" => {
            Kind::Text
        }
        _ => Kind::System,
    }
}

/// What's decided before the name matters. Availability comes first, as listing
/// a placeholder folder downloads it.
fn before_reading(availability: Availability, folder: bool) -> Option<Kind> {
    if availability != Availability::Local {
        return Some(Kind::NotDownloaded);
    }
    // The only folders that get here are packages.
    folder.then_some(Kind::System)
}

/// Which renderer `path` gets. Reads its metadata only.
pub fn classify(path: &Path) -> Kind {
    if needs_consent(path) {
        return Kind::NeedsAccess;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Kind::System;
    };
    if let Some(kind) = before_reading(Availability::of(&meta), meta.is_dir()) {
        return kind;
    }
    if path
        .file_name()
        .is_some_and(|name| is_env(&name.to_string_lossy()))
    {
        return Kind::Env;
    }
    let extension = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
        .or_else(|| {
            // `.gitignore` is all name, and `Makefile` has no extension.
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            Some(name.trim_start_matches('.').to_string())
        })
        .unwrap_or_default();
    match extension.as_str() {
        "makefile" | "dockerfile" | "license" | "readme" | "gemfile" => Kind::Text,
        extension => by_extension(extension),
    }
}

fn is_env(name: &str) -> bool {
    let name = name.to_lowercase();
    name == ".env" || name.starts_with(".env.") || name.ends_with(".env")
}

/// Where each line of an env file holds a value, by `str::lines`: after the
/// `=` of an assignment, commented out or not, and every line of a quoted value
/// that runs on past its own. A commented one ends with its line.
pub fn env_values(text: &str) -> Vec<Option<Range<usize>>> {
    let mut open: Option<char> = None;
    text.lines()
        .map(|line| {
            if let Some(quote) = open {
                if closes(line, quote) {
                    open = None;
                }
                return trimmed(line, 0);
            }
            let (start, commented) = assignment(line)?;
            let value = line[start..].trim_start();
            if let Some(quote) = value
                .chars()
                .next()
                .filter(|c| matches!(c, '"' | '\'' | '`'))
                && !closes(&value[1..], quote)
                && !commented
            {
                open = Some(quote);
            }
            trimmed(line, start)
        })
        .collect()
}

/// Where the value starts in `KEY=value`, `export KEY=value` or `# KEY=value`,
/// and whether it's commented out.
fn assignment(line: &str) -> Option<(usize, bool)> {
    let bare = line.trim_start_matches('\u{feff}').trim_start();
    let body = bare.trim_start_matches('#').trim_start();
    let body = body
        .strip_prefix("export")
        .filter(|rest| rest.starts_with([' ', '\t']))
        .unwrap_or(body)
        .trim_start();
    let (key, value) = body.split_once('=')?;
    let key = key.trim_end();
    let named = !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    named.then(|| (line.len() - value.len(), bare.starts_with('#')))
}

/// `line` from `start`, without the space around it, unless that's nothing.
fn trimmed(line: &str, start: usize) -> Option<Range<usize>> {
    let rest = &line[start..];
    let from = start + (rest.len() - rest.trim_start().len());
    let to = line.trim_end().len();
    (from < to).then_some(from..to)
}

/// Whether `text` holds an unescaped `quote`.
fn closes(text: &str, quote: char) -> bool {
    let mut escaped = false;
    for c in text.chars() {
        match c {
            '\\' if !escaped => escaped = true,
            c if c == quote && !escaped => return true,
            _ => escaped = false,
        }
    }
    false
}

/// The start of a text file, and whether there was more.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextFile {
    pub text: String,
    pub cut: bool,
}

/// Reads up to `TEXT_LIMIT`, refusing what isn't text, off the UI thread.
pub fn read_text(path: &Path) -> Result<TextFile, String> {
    cloud::without_downloads(|| {
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(TEXT_LIMIT + 1).read_to_end(&mut bytes))
            .map_err(|e| cloud::read_error(path, &e))?;
        let cut = bytes.len() as u64 > TEXT_LIMIT;
        bytes.truncate(TEXT_LIMIT as usize);
        if bytes.iter().take(8192).any(|byte| *byte == 0) {
            return Err("This file isn't text.".to_string());
        }
        Ok(TextFile {
            text: String::from_utf8_lossy(&bytes).into_owned(),
            cut,
        })
    })
}

/// Past this, formatted text isn't offered: deep nesting can indent a small
/// file into gigabytes.
const PRETTY_LIMIT: usize = 2 * TEXT_LIMIT as usize;

/// A JSON or XML file laid out one value or tag per line, when that differs
/// from the file. A file cut at `TEXT_LIMIT` doesn't parse, so it has none.
pub fn prettified(file: &TextFile, path: &Path) -> Option<String> {
    if file.cut {
        return None;
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let pretty = match extension.as_str() {
        "json" => pretty_json(&file.text)?,
        "xml" => pretty_xml(&file.text)?,
        _ => return None,
    };
    (pretty != file.text.trim_end()).then_some(pretty)
}

/// Reindents rather than reserializes, so numbers and key order stay as written.
fn pretty_json(text: &str) -> Option<String> {
    serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
    let mut out = String::with_capacity(text.len() * 2);
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.extend(std::iter::repeat_n("  ", depth));
    };
    let mut depth = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push('"');
                while let Some(c) = chars.next() {
                    out.push(c);
                    match c {
                        '\\' => out.extend(chars.next()),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '{' | '[' => {
                out.push(c);
                while chars.next_if(char::is_ascii_whitespace).is_some() {}
                match chars.next_if(|c| matches!(c, '}' | ']')) {
                    Some(close) => out.push(close),
                    None => {
                        depth += 1;
                        newline(&mut out, depth);
                    }
                }
            }
            '}' | ']' => {
                depth -= 1;
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(',');
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_ascii_whitespace() => {}
            c => out.push(c),
        }
        if out.len() > PRETTY_LIMIT {
            return None;
        }
    }
    Some(out)
}

fn pretty_xml(text: &str) -> Option<String> {
    use quick_xml::events::BytesText;
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(text);
    let mut writer = quick_xml::Writer::new_with_indent(Vec::new(), b' ', 2);
    // The reader splits text at each `&amp;`, so a run is trimmed as a whole.
    let mut run = String::new();
    // The reader takes an unclosed tag or a second root, so this checks for
    // one whole document itself.
    let (mut depth, mut roots) = (0usize, 0);
    loop {
        let event = reader.read_event().ok()?;
        match &event {
            Event::Text(text) => run.push_str(text),
            Event::GeneralRef(name) => run.push_str(&format!("&{};", &**name)),
            _ => {
                let trimmed = run.trim();
                if !trimmed.is_empty() {
                    if depth == 0 {
                        return None;
                    }
                    writer
                        .write_event(Event::Text(BytesText::from_escaped(trimmed)))
                        .ok()?;
                }
                run.clear();
                match &event {
                    Event::Eof => break,
                    Event::CData(_) if depth == 0 => return None,
                    Event::Start(_) => {
                        roots += usize::from(depth == 0);
                        depth += 1;
                    }
                    Event::Empty(_) => roots += usize::from(depth == 0),
                    Event::End(_) => depth = depth.checked_sub(1)?,
                    _ => {}
                }
                writer.write_event(event).ok()?;
                if writer.get_ref().len() > PRETTY_LIMIT {
                    return None;
                }
            }
        }
    }
    (depth == 0 && roots == 1).then_some(())?;
    String::from_utf8(writer.into_inner()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kind_comes_from_the_name_and_a_folder_from_the_disk() {
        let dir = tempfile::tempdir().unwrap();
        let kind = |name: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, "x").unwrap();
            classify(&path)
        };
        assert_eq!(kind("a.JPG"), Kind::Picture);
        assert_eq!(kind("a.gif"), Kind::Animated);
        assert_eq!(kind("a.mp4"), Kind::Video);
        assert_eq!(kind("a.webm"), Kind::Unplayable);
        assert_eq!(kind("a.mp3"), Kind::Audio);
        assert_eq!(kind("a.md"), Kind::Markdown);
        assert_eq!(kind("a.html"), Kind::Html);
        assert_eq!(kind("a.zip"), Kind::Zip);
        assert_eq!(kind("a.rs"), Kind::Text);
        assert_eq!(kind(".gitignore"), Kind::Text);
        assert_eq!(kind(".gitattributes"), Kind::Text);
        assert_eq!(kind("Makefile"), Kind::Text);
        assert_eq!(kind(".env"), Kind::Env);
        assert_eq!(kind(".env.local"), Kind::Env);
        assert_eq!(kind("prod.ENV"), Kind::Env);
        assert_eq!(kind(".envrc"), Kind::System);
        assert_eq!(kind("a.pdf"), Kind::System);
        assert_eq!(kind("a.unknown"), Kind::System);
        let app = dir.path().join("Kit.app");
        std::fs::create_dir(&app).unwrap();
        assert_eq!(classify(&app), Kind::System);
    }

    #[test]
    fn a_placeholder_never_picks_a_renderer_that_reads_it() {
        for availability in [
            Availability::Placeholder,
            Availability::Downloading,
            Availability::Unknown,
        ] {
            for folder in [true, false] {
                assert_eq!(
                    before_reading(availability, folder),
                    Some(Kind::NotDownloaded)
                );
            }
        }
        assert_eq!(
            before_reading(Availability::Local, true),
            Some(Kind::System)
        );
        assert_eq!(before_reading(Availability::Local, false), None);
    }

    #[test]
    fn env_values_cover_every_secret_and_nothing_else() {
        let text = "\u{feff}# UNFINISHED=\"x
FIRST=1
# Database
DB_URL=postgres://me:pw@host/db
export\tTOKEN = 'abc' # old
#OLD_KEY=sk-123
# UNFINISHED=\"x
EMPTY=
# Set this to a=b
KEY=\"-----BEGIN
middle \\\" still
end\"
AFTER=1";
        let values: Vec<_> = text
            .lines()
            .zip(env_values(text))
            .map(|(line, range)| range.map(|range| &line[range]))
            .collect();
        assert_eq!(
            values,
            [
                Some("\"x"),
                Some("1"),
                None,
                Some("postgres://me:pw@host/db"),
                Some("'abc' # old"),
                Some("sk-123"),
                Some("\"x"),
                None,
                None,
                Some("\"-----BEGIN"),
                Some("middle \\\" still"),
                Some("end\""),
                Some("1"),
            ]
        );
    }

    #[test]
    fn only_json_and_xml_that_change_get_prettified() {
        let pretty = |name: &str, text: &str| {
            let file = TextFile {
                text: text.into(),
                cut: false,
            };
            prettified(&file, Path::new(name))
        };
        assert_eq!(
            pretty(
                "a.JSON",
                r#"{"z":1.0e10,"a":[18446744073709551616,{}, []],"s":"a\"],{:"}"#
            )
            .as_deref(),
            Some(
                r#"{
  "z": 1.0e10,
  "a": [
    18446744073709551616,
    {},
    []
  ],
  "s": "a\"],{:"
}"#
            )
        );
        assert_eq!(pretty("a.json", "{\n  \"a\": 1\n}\n"), None);
        assert_eq!(pretty("a.json", "{\"a\":"), None);
        assert_eq!(pretty("a.txt", "{\"a\":1}"), None);
        assert_eq!(
            pretty(
                "a.xml",
                "<?xml version=\"1.0\"?><a x=\"1\"><!-- c --><b>t &amp; u</b><c/></a>"
            )
            .as_deref(),
            Some(
                "<?xml version=\"1.0\"?>\n<a x=\"1\">\n  <!-- c -->\n  <b>t &amp; u</b>\n  <c/>\n</a>"
            )
        );
        assert_eq!(pretty("a.xml", "<a><b></a>"), None);
        assert_eq!(pretty("a.xml", "<a><b/>"), None);
        assert_eq!(pretty("a.xml", "<a/><b/>"), None);
        assert_eq!(pretty("a.xml", "  loose  "), None);
        let deep = 3000;
        let json = format!("{}0{}", "[".repeat(deep), "]".repeat(deep));
        assert_eq!(pretty("a.json", &json), None);
        let xml = format!("{}{}", "<a>".repeat(deep), "</a>".repeat(deep));
        assert_eq!(pretty("a.xml", &xml), None);
        let cut = TextFile {
            text: "{\"a\":1}".into(),
            cut: true,
        };
        assert_eq!(prettified(&cut, Path::new("a.json")), None);
    }

    #[test]
    fn text_is_bounded_and_binary_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        std::fs::write(&path, vec![b'a'; TEXT_LIMIT as usize + 10]).unwrap();
        let read = read_text(&path).unwrap();
        assert!(read.cut);
        assert_eq!(read.text.len(), TEXT_LIMIT as usize);
        std::fs::write(&path, "short").unwrap();
        assert_eq!(
            read_text(&path).unwrap(),
            TextFile {
                text: "short".into(),
                cut: false
            }
        );
        std::fs::write(&path, [b'a', 0, b'b']).unwrap();
        assert!(read_text(&path).is_err());
    }
}
