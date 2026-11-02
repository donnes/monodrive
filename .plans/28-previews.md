# Built-in previews and editing

Status: steps 1 to 6 built (frame, system fallback, text, Markdown, pictures, video, audio, HTML, archives, folders). Replaces the `QLPreviewPanel` from [plan 06](06-quick-look.md), which stays behind "Open in Quick Look" until a person confirms parity.

Built differently from what's below, each to keep the first version small:

- Markdown is a small renderer on `pulldown-cmark`, not a port of Monocode's 1,800 lines. Tables read as plain rows.
- Pictures come from the system's generator (`monodrive_core::thumbnails`), not ImageIO directly. It reads HEIC and RAW and stops at the side asked for. Fit and 100% exist; pinch and pan by drag don't.
- A video's poster is drawn only until its first frame is ready. With the poster under the player layer, the first frame stayed on screen during playback, even though native views otherwise sit above GPUI's background. That wasn't explained, only avoided.
- Checked in a real window on 2026-11-02: a click inside `QLPreviewView` and inside `WKWebView`, then a real Space, reaches GPUI. A hostile page's script doesn't run. Playback advances the frame and the time.
- `examples/preview_window.rs` opens one real window on a file, to look at what headless snapshots can't draw.

## Goal

Space shows the selection inside Monodrive, drawn in Monodrive's style, with no second window. Monodrive renders the common kinds itself and never previews fewer kinds than Finder does. The preview is also where small edits happen: drawing on and cropping pictures, and trimming and cropping videos, without opening another app.

The panel has three problems today. It's Apple's window, so it can't follow `DESIGN.md`. Opening it spins a nested run loop until the first preview loads, which is why `quick_look.rs` has to open it from a task of its own. And the app can't add behavior to it, such as zoom or a Markdown view.

## Decisions

**Keep Apple's renderers for the long tail.** Office, iWork, PDF, fonts, 3D and third-party Quick Look plugins go through `QLPreviewView` embedded in our frame. `objc2-quick-look-ui` already has the binding. Reimplementing these would take years and lose plugins. "Replacing Quick Look" means replacing the panel and the common kinds, and the fallback keeps every other kind working from the first step.

**One native-view host.** Video, HTML and the fallback all need a native view above GPUI's Metal layer. Monocode already does this in `crates/monocode-gpui/src/monos/html.rs` (323 lines): a clip view, a flipped coordinate system, and a frame updated when the element paints. Port it once into `native_view.rs`, in the same spirit as `traffic_lights.rs`. Nothing GPUI draws can sit above a native view, so the frame's controls go beside the content and never over it.

**Video uses `AVPlayerLayer`, not GPUI's `surface()`.** GPUI's `surface` element exists, but it has three limits. It needs the app to repaint the whole window for every video frame, which goes against the rule on continuous repainting. The renderer asserts 8-bit `420f` pixel buffers, so HDR is lost. And it has no corner radii. A player layer composites without any GPUI repaint, keeps HDR and costs nothing while paused. It takes no mouse or key events (`hitTest` returns nil), so GPUI keeps the keyboard. GPUI draws the controls in a strip under the picture.

**A preview never starts a download or a prompt.** A cloud placeholder shows its icon and plan 18's download action. A file behind a privacy prompt says so, using the inspector's existing gate. A network file previews only because the user asked for that one file, and it stays cancellable.

## The frame

The maintainer picked this from drawn options on 2026-10-31; the mockups aren't in the repo. The preview takes over the pane commands act on, with a bar above and tools below. The sidebar, tabs and a second pane stay, and closing the sidebar gives more room. It appears in the next frame and needs no AppKit window.

Build one `PreviewView` entity with no knowledge of where it sits, so plan 19's preview column can reuse it later.

Keys stay as they are: Space and ⌘Y open and close, Esc closes, arrows move the selection and the preview follows. Several selected items page in the order `quick_look::Preview::of` already computes, which moves over unchanged. The header holds the name, position ("3 of 12"), Open and Close. Add the overlay's states to `examples/snapshot.rs`.

## Renderers

| Kind | How | Limits |
|---|---|---|
| Photos | ImageIO, with `CGImageSourceCreateThumbnailAtIndex` at the size drawn, then a sharper decode at 100% zoom. Handles HEIC and RAW, which `image` doesn't. Fit, 100%, pinch and pan. GIF and animated WebP play while shown | Never decode past the display size until zoomed: a 24 MP photo is 96 MB in BGRA. GPUI's atlas stops at 16,384 pixels a side |
| Video | `AVPlayer` and `AVPlayerLayer`, poster from [plan 29](29-video-thumbnails.md), play and pause, scrub, time and mute under the picture. Paused on open | `.webm` and `.mkv` don't open in AVFoundation (measured). They show the icon and "Open With" |
| Audio | `AVPlayer` with the same strip, plus artwork from the asset | Same formats as AVFoundation |
| Markdown | Port `markdown.rs` and `markdown_model.rs` (about 1,800 lines) on `pulldown-cmark`, leaving out Mermaid, streaming and notes. A toggle shows the source | Relative images load through the photo path. Remote images don't load. Links open only on click, in the default browser |
| HTML | `WKWebView` from `monos/html.rs`, with a non-persistent data store and a navigation delegate that refuses everything but the first load | JavaScript off and network blocked (`allowsContentJavaScript`, `WKContentRuleList`). Read access limited to the file's folder. A page is untrusted input |
| Text and code | Read a bounded prefix off the UI thread, sniff for binary, draw with `uniform_list`, highlight with Monocode's `syntax.rs` on `syntect` | First 5 MB, and it says so |
| Archives | `archives::Archive::open` as a read-only list, sharing plan 13's picked "browse a ZIP" rows | ZIP only, as core is today |
| Folders | Icon, item count and total, from the inspector's existing read | |
| PDF, Office, iWork, fonts, 3D, plugins, the rest | `QLPreviewView` | Native PDF only if a real need shows up |

## Tools

Space opens a read-only view, so a stray key can't change anything. Edit (E) shows the tools. Each tool is a typed step in a `Recipe` held in memory, so undo inside the session is a list pop, and Esc with unsaved changes asks first.

| Kind | Tools | Done by |
|---|---|---|
| Photos | Crop, rotate, flip, pen, highlighter, arrow, rectangle, ellipse, text, redact | GPUI draws the live marks over the picture. Core Graphics flattens the recipe into the saved file |
| Video | Trim, crop, rotate, mute, save a frame | `AVAssetExportSession`. Trim alone uses the passthrough preset, with no re-encode, so cuts snap to keyframes. Crop and rotate use an `AVVideoComposition` and re-encode |

**Crop on video uses a frozen frame.** Nothing GPUI draws can sit over a player layer, so choosing Crop pauses, draws the current frame as a GPUI image, and puts the handles over that. Scrubbing in crop mode fetches frames with `AVAssetImageGenerator`. Playing leaves crop mode and shows the result. This keeps the no-repaint player. Drawing over moving video would need `surface()` and isn't in this plan.

**Redact burns pixels.** It fills the region with solid color in the flattened file, because blur can be reversed. Saving also strips the metadata of a redacted picture, since an embedded thumbnail or location can leak what was hidden.

**Saving follows "the disk is the user's".**

- Save Copy is the default: `photo edited.jpg` beside the original, numbered when taken. It never overwrites.
- Replace Original moves the original to the Trash and puts the new file at its name, as one operation that Undo reverses.
- Save As picks a name, folder and format.
- Writing goes through the ops owner as a new `Request::SaveEdit`, under a hidden name and then renamed, as copies do. Video export is a job, with progress and cancel in the title bar ring, and it keeps running if the preview closes. Photos go through the same request and finish before the reply.
- A file that changed on disk since it opened refuses the save and says so. Read-only volumes offer only Save As.

Annotations are flattened on save. There's no sidecar file or layered format, so a saved mark can't be moved later, but the original stays intact with Save Copy and Replace Original. HEIC and JPEG re-encode at high quality, so a save isn't lossless. Crop-only JPEG saves don't get lossless handling in the first version.

**Agents.** Editing logic lives in `monodrive_core::edits`, with a serializable `Recipe`. MCP gets `edit_image` and `edit_video`, each taking a path and typed steps, and always writing a new file unless `replace` is set, under "Let agents change files". Freehand strokes are points, so agents can pass them, though arrows, boxes, crops and redactions are what they'd use.

**Not in this plan.** Filters and color adjustment, layers, multi-clip editing, video annotation, text and Markdown editing, and PDF markup. These turn a file manager into an editor app. The Recipe type can grow if one proves necessary.

## Speed

Proposed budgets for `docs/performance.md`. Measure them in step 1 and adjust before they bind.

| What | Budget |
|---|---|
| Space to the frame on screen, showing the grid's thumbnail or icon | Next frame |
| Sharp picture for a local 24 MP photo | 100 ms |
| Markdown or text of 1 MB, first frame | 50 ms |
| Holding ↓ through a folder | Never blocks, and only the newest item decodes |

A 6000×4000 JPEG took 22 ms through ImageIO's thumbnail path and 76 ms for a full decode, on an M3 Pro with synthetic content and a warm disk cache. Real photos are slower to decode, so the 100 ms needs checking on real files.

- All reads run on the background executor with a generation number. A result for an item that's no longer shown is dropped, and a running player or page is torn down.
- Images are released with `cx.drop_image` on close, as `thumbnails::release` does.
- `WKWebView` starts a web content process, so create it only when an HTML file is first shown. `AVPlayer` too.
- Video plays through the layer, so a playing video adds no GPUI frames. The scrub position repaints only the strip, about 4 times a second.

## Order

Each step ships alone.

1. **Frame and fallback.** `PreviewView`, `native_view.rs`, `QLPreviewView` for everything, Space routed to it. The panel stays until a person confirms parity, then `quick_look.rs` loses it. This step answers the riskiest question early, which is whether a native view that takes clicks gives the keys back.
2. **Text and Markdown.** The highest value for a developer's folders, and fully GPUI, so snapshots cover them.
3. **Photos.**
4. **Video and audio.** Needs plan 29's poster to avoid a blank frame.
5. **HTML.**
6. **Archives and folders.**
7. **Photo tools.** `edits`, `Request::SaveEdit` and the MCP tool, then crop, shapes, text and redact, then pen and highlighter, which need GPUI's `PathBuilder` for live strokes.
8. **Video tools.** Trim first, as it needs no re-encode, then crop, rotate and mute.

## Risks to settle in step 1

- **Focus.** A click inside `WKWebView` or `QLPreviewView` makes it first responder, and Space and the arrows then go to it. Handle this in `native_view.rs`, using the same forward-to-window approach as `quick_look.rs`'s `forward`, and check text selection still works.
- **Resize.** A native view's frame updates during paint, so it can trail GPUI by a frame during a live resize.
- **Rounded corners.** The clip view's layer handles them for native content, and the radius has to match `DESIGN.md`'s shapes.
- **Snapshots.** The headless renderer can't draw native views. Snapshots show the poster and the frame, so video, HTML and the fallback need a person to look, or an approved screen capture.

## Hit every surface

- **UI and MCP.** The plans README says UI-only presentation needs no MCP equivalent, and viewing qualifies, since agents read files directly. Editing changes files, so it has MCP tools. Core gains `preview::classify(path)` so the kind logic stays testable. Media facts (duration, dimensions, codec) go in `info.rs`, so Get Info and MCP `stat` get them too.
- **Entry points.** The existing `QuickLook` command keeps its keys, menu item and palette row. Add "Open in Quick Look" only if the fallback shows a gap.
- **Reverse states.** Esc, Space and the ✕ close. Audio and video stop on close.
- **Permissions.** Covered above. No renderer may touch a gated file before the gate check.

## Dependencies

`pulldown-cmark` and `syntect` (both MIT), and `objc2-av-foundation`, `objc2-web-kit`, `objc2-image-io`, `objc2-core-graphics`, each with narrow features. No new media library is needed for editing. Check them against `deny.toml`, and check the binary-size and launch effect, since `syntect`'s default syntax set is large. Load it lazily if launch moves.

## Verification

- `preview::classify` over names, kinds and availability, including placeholders, which must never pick a renderer that reads the file.
- `#[gpui::test]` over a temp folder for open, close, arrows following the selection, several selected items, and a stale decode being dropped.
- Markdown parsing and the 5 MB text cut, at the lowest layer.
- A hostile HTML fixture: a script, a remote image and a `file://` link outside its folder. None run or load. This needs a real web view, so it's a manual check until step 5 shows it can be automated.
- Editing, in core, on temp files: a recipe gives the expected pixels and dimensions, a rotated phone video crops the right region, redaction leaves no original pixels or embedded thumbnail, Save Copy numbers around a taken name, Replace Original undoes to the same inode, and a file changed since opening refuses the save.
- Snapshots before and after, dark and light. Rerun `scrolling` once, since the browser's render gains an overlay branch.
