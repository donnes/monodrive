//! How long a folder takes to list and to sort, at the sizes people have:
//! a few hundred items, a busy Downloads, and a folder of generated files.
//! `cargo bench -p monodrive-core --bench listing`

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use monodrive_core::entries::{Entry, Sort, SortBy, list_dir, sort};
use std::path::Path;

const SIZES: [usize; 3] = [1_000, 10_000, 100_000];

/// Names shaped like a real folder's: mixed case, numbered runs, a spread of
/// extensions, and one folder for every ten files.
fn name(i: usize) -> (String, bool) {
    const STEMS: [&str; 6] = [
        "IMG_",
        "report ",
        "Screenshot ",
        "notes-",
        "Invoice",
        "build",
    ];
    const EXTS: [&str; 6] = ["jpg", "pdf", "png", "md", "txt", "rs"];
    let stem = STEMS[i % STEMS.len()];
    if i.is_multiple_of(10) {
        (format!("{stem}{i}"), true)
    } else {
        (format!("{stem}{i}.{}", EXTS[i % EXTS.len()]), false)
    }
}

fn fill(dir: &Path, count: usize) {
    for i in 0..count {
        let (name, is_dir) = name(i);
        let path = dir.join(name);
        if is_dir {
            std::fs::create_dir(path).unwrap();
        } else {
            std::fs::write(path, i.to_le_bytes()).unwrap();
        }
    }
}

fn listing(c: &mut Criterion) {
    let mut group = c.benchmark_group("list_dir");
    group.sample_size(10);
    for count in SIZES {
        let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        fill(dir.path(), count);
        group.bench_with_input(BenchmarkId::from_parameter(count), dir.path(), |b, dir| {
            b.iter(|| list_dir(dir, false, Sort::default()).unwrap());
        });
    }
    group.finish();
}

fn sorting(c: &mut Criterion) {
    let dir = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    fill(dir.path(), SIZES[1]);
    // `list_dir` hands them back sorted by name, with equal sizes and near
    // equal dates: shuffle them and spread both, so no column starts sorted.
    let mut entries: Vec<Entry> = list_dir(dir.path(), false, Sort::default()).unwrap();
    let spread = |i: usize| (i as u64).wrapping_mul(2_654_435_761) % 1_000_000_007;
    for (i, entry) in entries.iter_mut().enumerate() {
        entry.size = spread(i) % 50_000_000;
        entry.modified = 1_700_000_000 + spread(i + 1) % 100_000_000;
    }
    entries.sort_by_key(|entry| spread(entry.size as usize));
    let mut group = c.benchmark_group("sort_10000");
    for by in [SortBy::Name, SortBy::Modified, SortBy::Size, SortBy::Kind] {
        let order = Sort {
            by,
            ..Sort::default()
        };
        group.bench_function(format!("{by:?}"), |b| {
            b.iter_batched_ref(
                || entries.clone(),
                |entries| sort(entries, order),
                BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, listing, sorting);
criterion_main!(benches);
