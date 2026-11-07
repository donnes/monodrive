// Flat stand-ins for the icons macOS draws. The site can't use Finder's own,
// so these keep the same shapes and colors without copying them.

export type Kind = "folder" | "document" | "image" | "video" | "code" | "archive" | "app" | "audio";

const page = (band: string, body = "") => `
<svg viewBox="0 0 32 32" aria-hidden="true">
  <path d="M8 3h11l7 7v17a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z" fill="#f2f2f0"/>
  <path d="M19 3v5a2 2 0 0 0 2 2h5z" fill="#cfcfcb"/>
  ${band ? `<rect x="6" y="20" width="20" height="6" fill="${band}"/>` : ""}
  ${body}
</svg>`;

const ICONS: Record<Kind, string> = {
  folder: `
<svg viewBox="0 0 32 32" aria-hidden="true">
  <path d="M3 8a2 2 0 0 1 2-2h7l2.5 2.5H27a2 2 0 0 1 2 2V12H3z" fill="#2f86e0"/>
  <rect x="3" y="10" width="26" height="17" rx="2.2" fill="#56a8f5"/>
  <rect x="3" y="10" width="26" height="1.2" rx=".6" fill="#8cc6ff"/>
</svg>`,
  document: page("#e5484d"),
  code: page(
    "",
    `<path d="M12.5 15.5 10 18l2.5 2.5M19.5 15.5 22 18l-2.5 2.5M17 14.5l-2 7" stroke="#6b6b70" stroke-width="1.4" fill="none" stroke-linecap="round" stroke-linejoin="round"/>`,
  ),
  archive: page(
    "",
    `<path d="M15 4h2v2h-2zM17 6h2v2h-2zM15 8h2v2h-2zM17 10h2v2h-2zM15 12h2v2h-2z" fill="#8a8a90"/><rect x="14.5" y="15" width="4" height="5" rx="1" fill="#8a8a90"/>`,
  ),
  image: `
<svg viewBox="0 0 32 32" aria-hidden="true">
  <rect x="3" y="6" width="26" height="20" rx="3" fill="#2a3d5c"/>
  <circle cx="22" cy="12" r="2.5" fill="#ffd36b"/>
  <path d="M3 22l7-7 6 6 4-4 9 9v.5A2.5 2.5 0 0 1 26.5 26h-21A2.5 2.5 0 0 1 3 23.5z" fill="#5fae7a"/>
</svg>`,
  video: `
<svg viewBox="0 0 32 32" aria-hidden="true">
  <rect x="3" y="6" width="26" height="20" rx="3" fill="#2b2b33"/>
  <path d="M14 12v8l7-4z" fill="#f2f2f0"/>
</svg>`,
  audio: `
<svg viewBox="0 0 32 32" aria-hidden="true">
  <rect x="4" y="4" width="24" height="24" rx="6" fill="#fa4d6a"/>
  <path d="M13 21.5V11l9-2v10.5" stroke="#fff" stroke-width="1.8" fill="none"/>
  <circle cx="11.5" cy="21.5" r="2" fill="#fff"/><circle cx="20.5" cy="19.5" r="2" fill="#fff"/>
</svg>`,
  app: `
<svg viewBox="0 0 32 32" aria-hidden="true">
  <rect x="4" y="4" width="24" height="24" rx="6" fill="#3a3a42"/>
  <rect x="9" y="9" width="14" height="14" rx="3" fill="none" stroke="#bdbdc4" stroke-width="1.6"/>
</svg>`,
};

export const icon = (kind: Kind) => ICONS[kind];

/** Monodrive's mark: the folder of squares from assets/prod/logo.svg, on a
 *  5 × 4 grid, with its white fading from the top left. */
export const MARK: { col: number; row: number; alpha: number }[] = [
  { col: 0, row: 0, alpha: 1 },
  { col: 1, row: 0, alpha: 0.89 },
  { col: 0, row: 1, alpha: 0.89 },
  { col: 1, row: 1, alpha: 0.77 },
  { col: 2, row: 1, alpha: 0.66 },
  { col: 3, row: 1, alpha: 0.54 },
  { col: 4, row: 1, alpha: 0.43 },
  { col: 0, row: 2, alpha: 0.77 },
  { col: 1, row: 2, alpha: 0.66 },
  { col: 2, row: 2, alpha: 0.54 },
  { col: 3, row: 2, alpha: 0.43 },
  { col: 4, row: 2, alpha: 0.31 },
  { col: 0, row: 3, alpha: 0.66 },
  { col: 1, row: 3, alpha: 0.54 },
  { col: 2, row: 3, alpha: 0.43 },
  { col: 3, row: 3, alpha: 0.31 },
  { col: 4, row: 3, alpha: 0.2 },
];

export const markSvg = (size = 20) => `
<svg width="${size}" height="${size}" viewBox="172 242 680 540" aria-hidden="true">
  ${MARK.map(
    (s) =>
      `<rect x="${172 + s.col * 140}" y="${242 + s.row * 140}" width="120" height="120" fill="currentColor" fill-opacity="${s.alpha}"/>`,
  ).join("")}
</svg>`;
