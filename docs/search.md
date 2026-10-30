# Search

The window, `find_files`, `grep`'s `filters` and saved searches all read one language, parsed by `monodrive-core`'s `search::Query`. A query prints back as canonical text (`type:doc` becomes `kind:document`, `size:>1MB` becomes `size:>1mb`), so chips can rebuild the search field, and saved searches store that text.

## Filters

| Token | Means |
|---|---|
| `kind:image,video` | Any of: image, video, audio, document, code, archive, font, app, folder, file, link. `type:` is the same. |
| `ext:rs,toml` | Any of these extensions. |
| `size:>10mb` | `>`, `>=`, `<`, `<=`, `a..b` with both ends included, or an exact size. Units are b, kb, mb, gb and tb, decimal as in Finder. |
| `modified:7d` | In the last 7 days (`<7d` is the same); `>7d` is longer ago. Units are h, d, w, mo (30 days) and y (365 days). |
| `modified:>=2026-01-01` | Whole local days: `today`, `yesterday` or a date, with the same comparisons as size. `>2026-01-01` starts the day after. |
| `in:~/Work` | Searches under that folder, ahead of the window's scope. A relative path starts from the scope's folder. |
| `contains:"fix me"` | Looks inside files for this text. The rest of the query picks the files read. Name search and `find_files` refuse it and point to `grep`. |

Anything else goes to fsearch as typed: fuzzy words, `'exact`, `!not`, globs such as `src/**/*.rs` and `!node_modules/`, and fsearch's own `re:` and `path:`. Repeating `kind:` or `ext:` widens the search. Every other filter narrows it. `kind:`, `ext:` and globs narrow each other's extensions, so `kind:image ext:png` and `*.png kind:image` are PNGs only. A search takes one `size:`, `modified:` and `in:`. Folder, file, link and app set the entry kind fsearch matches, so they can't be combined with another kind.

## How it maps onto fsearch

Filters translate onto fsearch's own query fields in `index.rs`, so fsearch does all the matching, for names and for content search alike. Kind groups are fsearch's `type:` extension lists rather than UTType conformance. The index holds names, and a UTType per hit would cost a Launch Services lookup each. Monodrive sets size and date bounds directly, because fsearch's own `size:` treats `>` as `>=` and its `mtime:` takes only ages. A scope is fsearch's `in:`: everything below the folder's real path, not the folder itself.

## Content search

`Index::grep` takes a pattern and a filter `Query` with the same meaning as above. fsearch keeps each line's first 400 bytes. `Caps` bounds files, lines per file and lines in all, and `complete` is false when fsearch's 250 ms budget ran out or a cap left matches out.

The window runs a `search::Search`: the text typed, whether it looks at names or contents, and its scope. Looking at contents, the plain words are one literal phrase and the filters pick the files. With only filters there's nothing to look for, so it's a name search. It waits 120 ms for the typing to stop, since it reads files, and a newer search or leaving drops it.

## Kept searches

Saved searches and recent searches live in `places.json` and hold a query, so a kept content search is stored with `contains:`, which `Search::stored` writes and `Search::from_stored` reads back, to plain words where those mean the same and as written where they don't, such as a phrase that reads as a filter. Saved searches have MCP tools. Recent ones are the window's own record, of results that stayed on screen for 2 seconds, so a step on the way to typing something isn't kept, and no tool reads them.
