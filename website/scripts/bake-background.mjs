#!/usr/bin/env node
// Bakes a picture the way the app's Appearance > Dither does, for the window
// demo's background. A port of `texture` in crates/monodrive/src/background.rs:
// the image shrinks to at most 2048 px, then each 2×2 cell is lit at full
// strength or dropped to 8% against a 4×4 Bayer threshold.
//
//   node scripts/bake-background.mjs <image> <out.webp>
//
// Needs ImageMagick (`magick`) and `cwebp` on the PATH.

import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const [source, out] = process.argv.slice(2);
if (!source || !out) {
  console.error("usage: node scripts/bake-background.mjs <image> <out.webp>");
  process.exit(1);
}

const BAYER = [
  [0, 8, 2, 10],
  [12, 4, 14, 6],
  [3, 11, 1, 9],
  [15, 7, 13, 5],
];

const dir = mkdtempSync(join(tmpdir(), "bake-"));
try {
  const raw = join(dir, "in.rgba");
  execFileSync("magick", [source, "-auto-orient", "-resize", "2048x2048>", "-depth", "8", `rgba:${raw}`]);
  const [width, height] = execFileSync("magick", [source, "-auto-orient", "-resize", "2048x2048>", "-format", "%w %h", "info:"])
    .toString()
    .split(" ")
    .map(Number);
  const input = readFileSync(raw);
  if (input.length !== width * height * 4) throw new Error(`expected ${width}×${height}, got ${input.length} bytes`);

  const output = Buffer.alloc(input.length);
  const at = (x, y) => (Math.min(y, height - 1) * width + Math.min(x, width - 1)) * 4;
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const p = at(Math.floor(x / 2) * 2 + 1, Math.floor(y / 2) * 2 + 1);
      const peak = Math.max(input[p], input[p + 1], input[p + 2]);
      const threshold = (BAYER[Math.floor(y / 2) % 4][Math.floor(x / 2) % 4] + 0.5) / 16;
      const gain = peak / 255 > threshold ? 255 / Math.max(peak, 1) : 0.08;
      const o = (y * width + x) * 4;
      for (let c = 0; c < 3; c++) output[o + c] = Math.min(255, Math.round(input[p + c] * gain));
      output[o + 3] = input[p + 3];
    }
  }

  const png = join(dir, "out.png");
  writeFileSync(join(dir, "out.rgba"), output);
  execFileSync("magick", ["-size", `${width}x${height}`, "-depth", "8", `rgba:${join(dir, "out.rgba")}`, png]);
  // Lossless: lossy compression would smear the 2 px cells the effect is made of.
  execFileSync("cwebp", ["-quiet", "-lossless", "-z", "9", png, "-o", out]);
  console.log(`${out}: ${width}×${height}`);
} finally {
  rmSync(dir, { recursive: true, force: true });
}
