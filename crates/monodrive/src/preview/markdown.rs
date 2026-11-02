//! Markdown as blocks a list draws one by one. Only what a file can say and a
//! pane can show: no scripts or raw HTML, remote images stay out, and a link
//! opens only when it's clicked.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use std::ops::Range;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading(u8, Inline),
    Paragraph(Inline),
    /// The marker ("•" or "3."), its depth, and the text.
    Item(String, usize, Inline),
    Quote(Inline),
    Code(String),
    Image(PathBuf),
    Rule,
}

/// Text with ranges of it styled, as byte offsets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inline {
    pub text: String,
    pub bold: Vec<Range<usize>>,
    pub italic: Vec<Range<usize>>,
    pub code: Vec<Range<usize>>,
    pub links: Vec<(Range<usize>, String)>,
}

/// Parses `source`; relative images resolve against `folder`.
pub fn parse(source: &str, folder: &Path) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut inline = Inline::default();
    let mut open: Vec<(usize, Tag)> = Vec::new();
    let mut lists: Vec<Option<u64>> = Vec::new();
    let mut quotes = 0usize;
    let mut code: Option<String> = None;
    // Inside an image, whose alt text isn't part of the paragraph.
    let mut images = 0usize;
    // An item's own text ends where a list inside it starts, or where it does.
    fn end_item(lists: &mut [Option<u64>], inline: &mut Inline, blocks: &mut Vec<Block>) {
        if inline.text.is_empty() {
            return;
        }
        let depth = lists.len().saturating_sub(1);
        let marker = match lists.last_mut() {
            Some(Some(n)) => {
                *n += 1;
                format!("{}.", *n - 1)
            }
            _ => "•".to_string(),
        };
        blocks.push(Block::Item(marker, depth, std::mem::take(inline)));
    }
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(source, options) {
        match event {
            Event::Start(Tag::CodeBlock(kind)) => {
                let _: CodeBlockKind = kind;
                code = Some(String::new());
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(text) = code.take() {
                    blocks.push(Block::Code(text.trim_end().to_string()));
                }
            }
            Event::Start(Tag::List(start)) => {
                end_item(&mut lists, &mut inline, &mut blocks);
                lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                lists.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => quotes += 1,
            Event::End(TagEnd::BlockQuote(_)) => quotes = quotes.saturating_sub(1),
            Event::Start(Tag::Image { dest_url, .. }) => {
                images += 1;
                if !dest_url.contains("://") {
                    let dest = dest_url.split(['#', '?']).next().unwrap_or_default();
                    blocks.push(Block::Image(folder.join(dest)));
                }
            }
            Event::End(TagEnd::Image) => images = images.saturating_sub(1),
            Event::Start(tag) => open.push((inline.text.len(), tag)),
            Event::End(end) => {
                let (start, tag) = open.pop().unwrap_or((0, Tag::Paragraph));
                let range = start..inline.text.len();
                match (&tag, end) {
                    (Tag::Strong, _) => inline.bold.push(range),
                    (Tag::Emphasis, _) => inline.italic.push(range),
                    (Tag::Link { dest_url, .. }, _) => {
                        inline.links.push((range, dest_url.to_string()))
                    }
                    (Tag::Heading { level, .. }, _) => {
                        let level = heading_level(*level);
                        blocks.push(Block::Heading(level, std::mem::take(&mut inline)));
                    }
                    (Tag::Paragraph, _) if open.iter().any(|(_, t)| matches!(t, Tag::Item)) => {}
                    (Tag::Paragraph, _) if !inline.text.is_empty() => {
                        let text = std::mem::take(&mut inline);
                        blocks.push(if quotes > 0 {
                            Block::Quote(text)
                        } else {
                            Block::Paragraph(text)
                        });
                    }
                    (Tag::Item, _) => end_item(&mut lists, &mut inline, &mut blocks),
                    (Tag::TableRow | Tag::TableHead, _) => {
                        blocks.push(Block::Paragraph(std::mem::take(&mut inline)));
                    }
                    (Tag::TableCell, _) => inline.text.push_str("   "),
                    _ => {}
                }
            }
            Event::Text(_) if images > 0 => {}
            Event::Text(text) => match &mut code {
                Some(code) => code.push_str(&text),
                None => inline.text.push_str(&text),
            },
            Event::Code(text) => {
                let start = inline.text.len();
                inline.text.push_str(&text);
                inline.code.push(start..inline.text.len());
            }
            Event::SoftBreak | Event::HardBreak => inline.text.push(' '),
            Event::Rule => blocks.push(Block::Rule),
            Event::TaskListMarker(done) => {
                inline.text.push_str(if done { "☑ " } else { "☐ " });
            }
            // Raw HTML is left out, and so is whatever it might try.
            Event::Html(_) | Event::InlineHtml(_) | Event::FootnoteReference(_) => {}
            _ => {}
        }
    }
    blocks
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        _ => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_reads_as_blocks_with_styled_ranges() {
        let blocks = parse(
            "# Title\n\nSome **bold** and `code` and [a link](https://x.y).\n\n- one\n- two\n\n1. first\n\n```rust\nfn a() {}\n```\n\n> said\n\n---\n",
            Path::new("/d"),
        );
        let Block::Paragraph(para) = &blocks[1] else {
            panic!("{blocks:?}");
        };
        assert_eq!(para.text, "Some bold and code and a link.");
        assert_eq!(&para.text[para.bold[0].clone()], "bold");
        assert_eq!(&para.text[para.code[0].clone()], "code");
        assert_eq!(para.links[0].1, "https://x.y");
        assert!(matches!(&blocks[0], Block::Heading(1, title) if title.text == "Title"));
        assert!(matches!(&blocks[2], Block::Item(m, 0, t) if m == "•" && t.text == "one"));
        assert!(matches!(&blocks[4], Block::Item(m, 0, t) if m == "1." && t.text == "first"));
        assert_eq!(blocks[5], Block::Code("fn a() {}".into()));
        assert!(matches!(&blocks[6], Block::Quote(t) if t.text == "said"));
        assert_eq!(blocks[7], Block::Rule);
    }

    #[test]
    fn a_list_inside_an_item_keeps_its_own_rows() {
        let blocks = parse("- Boots\n  - Laces\n- [x] Passport\n", Path::new("/d"));
        let rows: Vec<(usize, &str)> = blocks
            .iter()
            .map(|block| match block {
                Block::Item(_, depth, text) => (*depth, text.text.as_str()),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(rows, [(0, "Boots"), (1, "Laces"), (0, "☑ Passport")]);
    }

    #[test]
    fn remote_images_and_raw_html_stay_out() {
        let blocks = parse(
            "![a](pics/a.png)\n\n![b](https://x.y/b.png)\n\n<script>alert(1)</script>\n",
            Path::new("/d"),
        );
        assert_eq!(blocks[0], Block::Image(PathBuf::from("/d/pics/a.png")));
        assert!(!format!("{blocks:?}").contains("x.y"));
        assert!(!format!("{blocks:?}").contains("alert"));
        // Nor does an image's alt text join the words around it.
        let blocks = parse("Before ![alt](https://x.y/b.png) after\n", Path::new("/d"));
        assert!(matches!(&blocks[0], Block::Paragraph(t) if t.text == "Before  after"));
    }
}
