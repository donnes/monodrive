//! What a preview shows for a path, and the bounded read behind text. Deciding
//! never opens the file, so a placeholder or a gated file can't pick a renderer
//! that reads it.

use crate::cloud::{self, Availability};
use crate::sizes::needs_consent;
use std::io::Read;
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
    Zip,
    Folder,
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
        | "pl" | "r" | "vue" | "svelte" | "gitignore" | "lock" => Kind::Text,
        _ => Kind::System,
    }
}

/// Which renderer `path` gets. Reads its metadata only.
pub fn classify(path: &Path) -> Kind {
    if needs_consent(path) {
        return Kind::NeedsAccess;
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return Kind::System;
    };
    // Before anything reads it: listing a placeholder folder downloads it.
    if Availability::of(&meta) != Availability::Local {
        return Kind::NotDownloaded;
    }
    if meta.is_dir() {
        return if crate::info::is_package(path) {
            Kind::System
        } else {
            Kind::Folder
        };
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
        assert_eq!(kind("Makefile"), Kind::Text);
        assert_eq!(kind("a.pdf"), Kind::System);
        assert_eq!(kind("a.unknown"), Kind::System);
        assert_eq!(classify(dir.path()), Kind::Folder);
        let app = dir.path().join("Kit.app");
        std::fs::create_dir(&app).unwrap();
        assert_eq!(classify(&app), Kind::System);
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
