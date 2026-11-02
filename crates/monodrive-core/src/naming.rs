//! Names read from what text-based files say. No model: every source here is
//! under 2 ms a file, and the fastest local model measured took over 600 ms
//! (see `.plans/30-local-organize.md`). A name goes through the same checks as
//! any rename, and nothing treats a file's text as an instruction.

use crate::cloud;
use crate::ops::check_name;
use crate::sizes::needs_consent;
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSString, NSURL};
use objc2_pdf_kit::{PDFDocument, PDFDocumentTitleAttribute};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

/// How much of a file is read, which is enough for a title or a first sentence.
const READ: u64 = 4096;
/// The longest a suggested name gets before its extension.
const LONGEST: usize = 60;
const TITLE_CHARS: std::ops::RangeInclusive<usize> = 3..=80;
/// How many words of a first sentence make a name.
const PHRASE_WORDS: usize = 8;
const NOISE: [&str; 5] = ["final", "copy", "draft", "new", "untitled"];

/// How a suggestion is written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Style {
    /// `residential-lease-agreement.pdf`
    #[default]
    Hyphens,
    /// `Residential lease agreement.pdf`
    Sentence,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Markdown,
    Html,
    Pdf,
    Table,
    Text,
    Code,
}

impl Format {
    fn of(extension: &str) -> Option<Self> {
        Some(match extension {
            "md" | "markdown" | "mdown" => Self::Markdown,
            "html" | "htm" | "xhtml" => Self::Html,
            "pdf" => Self::Pdf,
            "csv" | "tsv" => Self::Table,
            "txt" | "text" | "" => Self::Text,
            "rs" | "py" | "js" | "jsx" | "ts" | "tsx" | "go" | "c" | "h" | "cc" | "cpp"
            | "swift" | "java" | "kt" | "rb" | "sh" | "zsh" | "css" | "sql" => Self::Code,
            _ => return None,
        })
    }
}

/// A new name for `path`, keeping its extension, or why it keeps its old one.
/// Never downloads a placeholder or opens what could show a privacy prompt.
pub fn suggest(path: &Path, style: Style) -> Result<String, String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.is_file() {
        return Err("Only files get suggestions.".into());
    }
    if needs_consent(path) {
        return Err("Needs Full Disk Access to read.".into());
    }
    cloud::refuse_placeholder(path)?;
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default();
    let format = Format::of(&extension.to_lowercase())
        .ok_or("Only text-based files get suggestions.".to_string())?;
    let title = if format == Format::Pdf {
        cloud::without_downloads(|| pdf(path))?
    } else {
        let text = cloud::without_downloads(|| start_of(path))?;
        from_text(&text, format)
    };
    let stem = title
        .and_then(|title| clean(&title, style))
        .ok_or("It has no text that reads as a name.".to_string())?;
    let name = match extension {
        "" => stem,
        extension => format!("{stem}.{extension}"),
    };
    check_name(&name)?;
    Ok(name)
}

fn start_of(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(READ).read_to_end(&mut bytes))
        .map_err(|e| cloud::read_error(path, &e))?;
    if bytes.contains(&0) {
        return Err("It isn't text.".into());
    }
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        // The read may have stopped inside a character.
        Err(e) => {
            let valid = e.utf8_error().valid_up_to();
            if e.as_bytes().len() - valid > 3 {
                return Err("It isn't text.".into());
            }
            Ok(String::from_utf8_lossy(&e.as_bytes()[..valid]).into_owned())
        }
    }
}

fn pdf(path: &Path) -> Result<Option<String>, String> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    // SAFETY: a document is used only on this thread, and the title key is a
    // constant of the framework.
    let (title, text) = unsafe {
        let document: Option<Retained<PDFDocument>> =
            PDFDocument::initWithURL(PDFDocument::alloc(), &url);
        let document = document.ok_or("It can't be read as a PDF.")?;
        let title = document
            .documentAttributes()
            .and_then(|attributes| {
                let key: &AnyObject = PDFDocumentTitleAttribute.as_ref();
                attributes.objectForKey(key)
            })
            .and_then(|title| title.downcast::<NSString>().ok())
            .map(|title| title.to_string());
        let text = document
            .pageAtIndex(0)
            .and_then(|page| page.string())
            .map(|text| text.to_string());
        (title, text)
    };
    Ok(title
        .filter(|title| reads_as_title(title) && !looks_generated(title))
        .or_else(|| from_text(&text?, Format::Text)))
}

/// What a tool leaves in a PDF's title when nobody wrote one.
fn looks_generated(title: &str) -> bool {
    let lower = title.to_lowercase();
    ["microsoft word", ".doc", ".pdf", "untitled"]
        .iter()
        .any(|mark| lower.contains(mark))
}

fn from_text(text: &str, format: Format) -> Option<String> {
    let own = match format {
        Format::Markdown => markdown_title(text),
        Format::Html => html_title(text),
        Format::Code => return doc_comment(text),
        Format::Table => return table_header(text),
        Format::Text | Format::Pdf => None,
    };
    own.or_else(|| {
        let body = match format {
            Format::Html => strip_tags(text),
            _ => text.to_string(),
        };
        title_line(&body).or_else(|| first_phrase(&body))
    })
}

fn markdown_title(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next().is_some_and(|line| line.trim() == "---") {
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
            if let Some(title) = line.strip_prefix("title:") {
                let title = title.trim().trim_matches(['"', '\'']);
                if !title.is_empty() {
                    return Some(title.to_string());
                }
            }
        }
    }
    text.lines()
        .find_map(|line| line.strip_prefix("# "))
        .map(|heading| heading.trim().to_string())
}

fn html_title(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open = start + lower[start..].find('>')? + 1;
    let close = open + lower[open..].find("</title")?;
    let title = text[open..close].trim();
    (!title.is_empty()).then(|| title.to_string())
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' => {
                inside = false;
                out.push('\n');
            }
            c if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

/// The first comment, if the file starts with one; a first line of code says
/// too little to name a file.
fn doc_comment(text: &str) -> Option<String> {
    let line = text
        .lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with("#!"))?
        .trim();
    let said = ["//!", "///", "//", "/**", "/*", "\"\"\"", "#", "--"]
        .iter()
        .find_map(|mark| line.strip_prefix(mark))?;
    let said = said
        .trim()
        .trim_end_matches("*/")
        .trim_end_matches("\"\"\"");
    let said = said.trim();
    first_phrase(said).filter(|phrase| reads_as_title(phrase))
}

/// A table's columns say what it holds.
fn table_header(text: &str) -> Option<String> {
    let header = text.lines().next()?;
    let columns: Vec<&str> = header
        .split([',', '\t'])
        .map(|column| column.trim().trim_matches('"'))
        .filter(|column| !column.is_empty())
        .take(4)
        .collect();
    let joined = columns.join(" ");
    reads_as_title(&joined).then_some(joined)
}

/// The first line near the top that a person would call a title.
fn title_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(5)
        .find(|line| reads_as_title(line) && !ends_a_sentence(line))
        .map(str::to_string)
}

fn ends_a_sentence(line: &str) -> bool {
    line.ends_with(['.', ',', ';', ':'])
}

fn reads_as_title(line: &str) -> bool {
    let chars = line.chars().count();
    let letters = line.chars().filter(|c| c.is_alphabetic()).count();
    TITLE_CHARS.contains(&chars) && letters * 2 >= chars && letters >= 3 && !is_date_or_page(line)
}

fn is_date_or_page(line: &str) -> bool {
    let lower = line.to_lowercase();
    let without_digits: String = lower.chars().filter(|c| !c.is_ascii_digit()).collect();
    let words: Vec<&str> = without_digits
        .split(|c: char| !c.is_alphabetic())
        .filter(|word| !word.is_empty())
        .collect();
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let digits = lower.chars().any(|c| c.is_ascii_digit());
    match words.as_slice() {
        ["page", ..] if digits => true,
        ["page"] | ["of"] => true,
        [month] if digits => MONTHS.iter().any(|m| month.starts_with(m)),
        _ => false,
    }
}

/// The start of the first sentence that has words in it.
fn first_phrase(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|line| {
        let letters = line.chars().filter(|c| c.is_alphabetic()).count();
        letters >= 3 && letters * 2 >= line.chars().count()
    })?;
    let sentence = line.split(['.', '!', '?']).next().unwrap_or(line);
    let words: Vec<&str> = sentence.split_whitespace().take(PHRASE_WORDS).collect();
    (!words.is_empty()).then(|| words.join(" "))
}

/// The text as a stem: only what a name can hold, without the words that say
/// nothing, cut at a word, and written in `style`. `None` if nothing is left.
fn clean(title: &str, style: Style) -> Option<String> {
    let words: Vec<String> = title
        .split(|c: char| c.is_whitespace() || "/\\:|_".contains(c))
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .chars()
                .filter(|c| c.is_alphanumeric() || "-'&".contains(*c))
                .collect::<String>()
        })
        .filter(|word| !word.is_empty() && !says_nothing(word))
        .collect();
    let mut kept: Vec<String> = Vec::new();
    let mut length = 0;
    for word in words {
        length += word.chars().count() + usize::from(!kept.is_empty());
        if length > LONGEST {
            break;
        }
        kept.push(word);
    }
    if kept.is_empty() {
        return None;
    }
    Some(match style {
        Style::Hyphens => kept.join("-").to_lowercase(),
        Style::Sentence => {
            let joined = kept.join(" ");
            let mut chars = joined.chars();
            let first: String = chars.next()?.to_uppercase().collect();
            first + chars.as_str()
        }
    })
}

/// `final`, `copy`, and version marks like `v2`.
fn says_nothing(word: &str) -> bool {
    let lower = word.to_lowercase();
    let version = lower
        .strip_prefix('v')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
    version || NOISE.contains(&lower.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(file: &str, contents: &[u8], style: Style) -> Result<String, String> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file);
        std::fs::write(&path, contents).unwrap();
        suggest(&path, style)
    }

    fn hyphens(file: &str, contents: &str) -> Result<String, String> {
        named(file, contents.as_bytes(), Style::Hyphens)
    }

    #[test]
    fn each_source_is_tried_in_order() {
        // The format's own title wins over a heading or the first line.
        let front = "---\ntitle: \"Quarterly Plan\"\n---\n# Other heading\n";
        assert_eq!(hyphens("a.md", front).unwrap(), "quarterly-plan.md");
        assert_eq!(
            hyphens("a.md", "intro\n# Trip Notes\n").unwrap(),
            "trip-notes.md"
        );
        let page = "<html><head><TITLE>Lease Terms</TITLE></head><h1>Other</h1></html>";
        assert_eq!(hyphens("a.html", page).unwrap(), "lease-terms.html");
        // Then a line that reads as a title, skipping dates and page numbers.
        let invoice = "Page 1 of 3\nMarch 12, 2026\nInvoice 4471\nAmount due: 300.\n";
        assert_eq!(hyphens("scan.txt", invoice).unwrap(), "invoice-4471.txt");
        // Then the start of the first sentence, cut at a word.
        let sentence =
            "This agreement is made between the landlord and the tenant named below, in full.\n";
        assert_eq!(
            hyphens("a.txt", sentence).unwrap(),
            "this-agreement-is-made-between-the-landlord-and.txt"
        );
    }

    #[test]
    fn code_only_gets_a_name_from_a_leading_comment() {
        let commented = "// Parses the budget file.\nfn main() {}\n";
        assert_eq!(
            hyphens("a.rs", commented).unwrap(),
            "parses-the-budget-file.rs"
        );
        assert!(hyphens("a.rs", "fn main() {}\n").is_err());
    }

    #[test]
    fn a_table_is_named_for_its_columns() {
        assert_eq!(
            hyphens("a.csv", "name,email,total\nAda,a@b.c,3\n").unwrap(),
            "name-email-total.csv"
        );
    }

    #[test]
    fn names_are_safe_whatever_the_text_says() {
        // A slash can't move the file, or dots leave its folder.
        assert_eq!(
            hyphens("a.txt", "../../.ssh/authorized keys\n").unwrap(),
            "ssh-authorized-keys.txt"
        );
        // Words that say nothing go, and the extension keeps its case.
        assert_eq!(
            named("a.TXT", b"Budget FINAL v2 copy\n", Style::Sentence).unwrap(),
            "Budget.TXT"
        );
        // An overlong line is cut at a word.
        let long = "word ".repeat(40);
        let name = hyphens("a.txt", &format!("{long}\n")).unwrap();
        assert!(name.len() <= LONGEST + ".txt".len(), "{name}");
        assert!(name.ends_with("word.txt"), "{name}");
    }

    #[test]
    fn what_has_nothing_to_say_keeps_its_name() {
        assert!(named("a.txt", &[1, 0, 2, 0, 3], Style::Hyphens).is_err());
        assert!(hyphens("a.txt", "$$$ 1234 %%% ### 99\n").is_err());
        assert!(hyphens("a.txt", "").is_err());
        assert!(hyphens("a.jpg", "Looks like a title\n").is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(suggest(dir.path(), Style::Hyphens).is_err());
    }

    #[test]
    fn a_pdf_uses_its_title_then_its_first_page() {
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let name = |file: &str| suggest(&fixtures.join(file), Style::Hyphens).unwrap();
        assert_eq!(name("titled.pdf"), "annual-report.pdf");
        assert_eq!(name("text-layer.pdf"), "residential-lease-agreement.pdf");
    }

    #[test]
    fn a_character_cut_by_the_read_limit_is_still_text() {
        let text = format!("Résumé of the year\n{}", "é".repeat(READ as usize));
        assert_eq!(hyphens("a.txt", &text).unwrap(), "résumé-of-the-year.txt");
    }
}
