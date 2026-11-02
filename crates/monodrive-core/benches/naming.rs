//! Holds Suggest Names to its gate of 50 ms a file over a mix of text files.

use criterion::{Criterion, criterion_group, criterion_main};
use monodrive_core::naming::{Style, suggest};

fn suggesting(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        (
            "notes.md",
            "# Trip notes\n\nWe left on Tuesday.\n".repeat(200),
        ),
        ("page.html", "<html><title>Lease</title></html>".repeat(200)),
        ("scan.txt", "Invoice 4471\nAmount due.\n".repeat(2000)),
        (
            "main.rs",
            "// Parses the budget.\nfn main() {}\n".repeat(200),
        ),
        ("data.csv", "name,email,total\nAda,a@b.c,3\n".repeat(2000)),
    ];
    let paths: Vec<_> = files
        .iter()
        .map(|(name, text)| {
            let path = dir.path().join(name);
            std::fs::write(&path, text).unwrap();
            path
        })
        .collect();
    c.bench_function("suggest_five_text_files", |b| {
        b.iter(|| {
            for path in &paths {
                suggest(path, Style::Hyphens).unwrap();
            }
        })
    });
}

criterion_group!(benches, suggesting);
criterion_main!(benches);
