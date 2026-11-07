import type { Kind } from "./icons";

// Sample files for the demos. They make no claim about anyone's disk; the
// filter syntax is the real one from `find_files` in monodrive-core's MCP.

export type Sample = {
  name: string;
  folder: string;
  kind: Kind;
  bytes: number;
  /** Days since it was modified. */
  age: number;
};

const KB = 1024;
const MB = KB * 1024;
const GB = MB * 1024;

export const SAMPLES: Sample[] = [
  { name: "Invoice 2026-10 Northwind.pdf", folder: "~/Documents/Invoices", kind: "document", bytes: 182 * KB, age: 6 },
  { name: "Invoice 2026-09 Northwind.pdf", folder: "~/Documents/Invoices", kind: "document", bytes: 176 * KB, age: 37 },
  { name: "Invoice template.pages", folder: "~/Documents/Invoices", kind: "document", bytes: 1.2 * MB, age: 210 },
  { name: "Lease agreement signed.pdf", folder: "~/Documents/Home", kind: "document", bytes: 2.4 * MB, age: 402 },
  { name: "Tax return 2025.pdf", folder: "~/Documents/Taxes", kind: "document", bytes: 860 * KB, age: 190 },
  { name: "Quarterly report Q3.key", folder: "~/Documents/Work", kind: "document", bytes: 48 * MB, age: 21 },
  { name: "Roadmap notes.md", folder: "~/Documents/Work", kind: "code", bytes: 9 * KB, age: 2 },
  { name: "main.rs", folder: "~/Projects/monodrive/crates/monodrive/src", kind: "code", bytes: 14 * KB, age: 1 },
  { name: "browser.rs", folder: "~/Projects/monodrive/crates/monodrive/src", kind: "code", bytes: 310 * KB, age: 0 },
  { name: "index.rs", folder: "~/Projects/monodrive/crates/monodrive-core/src", kind: "code", bytes: 96 * KB, age: 3 },
  { name: "mcp.rs", folder: "~/Projects/monodrive/crates/monodrive-core/src", kind: "code", bytes: 188 * KB, age: 0 },
  { name: "Cargo.toml", folder: "~/Projects/monodrive", kind: "code", bytes: 1.3 * KB, age: 9 },
  { name: "App.tsx", folder: "~/Projects/garden/src", kind: "code", bytes: 6 * KB, age: 4 },
  { name: "package.json", folder: "~/Projects/garden", kind: "code", bytes: 2 * KB, age: 4 },
  { name: "Screenshot 2026-11-02 at 09.14.png", folder: "~/Desktop", kind: "image", bytes: 3.1 * MB, age: 4 },
  { name: "Screenshot 2026-11-05 at 18.40.png", folder: "~/Desktop", kind: "image", bytes: 2.7 * MB, age: 1 },
  { name: "Lisbon rooftop.heic", folder: "~/Pictures/2026/Lisbon", kind: "image", bytes: 4.4 * MB, age: 58 },
  { name: "Lisbon tram.heic", folder: "~/Pictures/2026/Lisbon", kind: "image", bytes: 3.9 * MB, age: 58 },
  { name: "Avatar.png", folder: "~/Pictures", kind: "image", bytes: 640 * KB, age: 300 },
  { name: "Lisbon day 2.mov", folder: "~/Movies", kind: "video", bytes: 2.8 * GB, age: 57 },
  { name: "Screen recording demo.mov", folder: "~/Movies", kind: "video", bytes: 1.4 * GB, age: 3 },
  { name: "Conference talk.mp4", folder: "~/Movies/Talks", kind: "video", bytes: 6.1 * GB, age: 120 },
  { name: "Voice memo 14.m4a", folder: "~/Music/Memos", kind: "audio", bytes: 12 * MB, age: 11 },
  { name: "Podcast ep 31.mp3", folder: "~/Music/Podcasts", kind: "audio", bytes: 84 * MB, age: 15 },
  { name: "Xcode_26.xip", folder: "~/Downloads", kind: "archive", bytes: 11.8 * GB, age: 30 },
  { name: "fonts.zip", folder: "~/Downloads", kind: "archive", bytes: 22 * MB, age: 44 },
  { name: "Monodrive-arm64.dmg", folder: "~/Downloads", kind: "archive", bytes: 18 * MB, age: 2 },
  { name: "backup-2026-10.tar.gz", folder: "~/Backups", kind: "archive", bytes: 3.3 * GB, age: 36 },
  { name: "Invoices", folder: "~/Documents", kind: "folder", bytes: 0, age: 6 },
  { name: "monodrive", folder: "~/Projects", kind: "folder", bytes: 0, age: 0 },
  { name: "garden", folder: "~/Projects", kind: "folder", bytes: 0, age: 4 },
  { name: "Lisbon", folder: "~/Pictures/2026", kind: "folder", bytes: 0, age: 58 },
  { name: "node_modules", folder: "~/Projects/garden", kind: "folder", bytes: 0, age: 4 },
];

export const formatBytes = (bytes: number) => {
  if (!bytes) return "--";
  const units = ["bytes", "KB", "MB", "GB"];
  let value = bytes;
  let unit = 0;
  // Finder counts in powers of 1000.
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit++;
  }
  return `${value < 10 && unit ? value.toFixed(1) : Math.round(value)} ${units[unit]}`;
};

const KINDS = new Set<Kind>(["folder", "document", "image", "video", "code", "archive", "app", "audio"]);

const extOf = (name: string) => {
  const dot = name.lastIndexOf(".");
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
};

const parseSize = (text: string) => {
  const m = /^([<>]=?)?(\d+(?:\.\d+)?)(kb|mb|gb)?$/.exec(text);
  if (!m) return null;
  const scale = m[3] === "gb" ? GB : m[3] === "mb" ? MB : m[3] === "kb" ? KB : 1;
  return { op: m[1] ?? ">=", bytes: Number(m[2]) * scale };
};

/** Matches `query` as a subsequence of `name`, returning the matched indices,
 *  or null. Earlier and adjacent matches score higher. */
const fuzzy = (name: string, query: string) => {
  const hay = name.toLowerCase();
  const hits: number[] = [];
  let from = 0;
  let score = 0;
  for (const ch of query) {
    const at = hay.indexOf(ch, from);
    if (at < 0) return null;
    score += at === from ? 3 : at === 0 || hay[at - 1] === " " ? 2 : 1;
    hits.push(at);
    from = at + 1;
  }
  return { hits, score };
};

export type Hit = { file: Sample; hits: number[]; score: number };

/** A small subset of `find_files`: fuzzy words plus kind:, ext:, size: and modified:Nd. */
export const search = (query: string, files: Sample[] = SAMPLES): Hit[] => {
  const words: string[] = [];
  const tests: ((f: Sample) => boolean)[] = [];
  for (const token of query.toLowerCase().split(/\s+/).filter(Boolean)) {
    const [key, value] = token.includes(":") ? token.split(":", 2) : ["", token];
    if (key === "kind" && KINDS.has(value as Kind)) tests.push((f) => f.kind === value);
    else if (key === "ext" && value) tests.push((f) => extOf(f.name) === value.replace(/^\./, ""));
    else if (key === "size") {
      const size = parseSize(value);
      if (size) tests.push((f) => (size.op.startsWith("<") ? f.bytes <= size.bytes : f.bytes >= size.bytes));
    } else if (key === "modified") {
      const days = /^<?(\d+)d$/.exec(value);
      if (days) tests.push((f) => f.age <= Number(days[1]));
    } else if (!key) words.push(value);
  }
  if (!words.length && !tests.length) return [];
  const results: Hit[] = [];
  for (const file of files) {
    if (!tests.every((t) => t(file))) continue;
    let hits: number[] = [];
    let score = 0;
    let ok = true;
    for (const word of words) {
      const m = fuzzy(file.name, word);
      if (!m) {
        ok = false;
        break;
      }
      hits = hits.concat(m.hits);
      score += m.score;
    }
    if (ok) results.push({ file, hits, score: score - file.age / 1000 });
  }
  return results.sort((a, b) => b.score - a.score);
};

export const highlight = (name: string, hits: number[]) => {
  const set = new Set(hits);
  let out = "";
  for (let i = 0; i < name.length; i++) {
    const ch = name[i].replace(/[&<>"]/g, (c) => `&#${c.charCodeAt(0)};`);
    out += set.has(i) ? `<b>${ch}</b>` : ch;
  }
  return out;
};
