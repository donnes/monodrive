//! How the app words sizes, counts and errors.

use std::path::Path;

/// Finder's decimal units.
pub fn format_bytes(size: u64) -> String {
    let bytes = size as f64;
    match size {
        0..1_000 => format!("{size} bytes"),
        1_000..1_000_000 => format!("{:.0} KB", bytes / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes / 1e6),
        _ => format!("{:.2} GB", bytes / 1e9),
    }
}

/// `n` with thousands separated, as Finder writes counts.
pub(crate) fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `why` without the folders it names, which the person can see, and with
/// the home folder as ~.
pub(crate) fn short_reason<'a>(why: &str, folders: impl IntoIterator<Item = &'a Path>) -> String {
    let mut why = why.to_string();
    for folder in folders {
        why = why.replace(&format!("{}/", folder.display()), "");
    }
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() => why.replace(&home, "~"),
        _ => why,
    }
}

/// "1 item", "3 items".
pub(crate) fn count(n: usize, what: &str) -> String {
    match n {
        1 => format!("1 {what}"),
        n => format!("{n} {what}s"),
    }
}
