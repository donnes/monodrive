# LocalSend

Status: done. Sending to and receiving from real phones and the LocalSend app need a person to check.

Decisions made while building it:

- Opt-in: Settings › General › Use LocalSend, off by default. Off, nothing of it shows or runs, no key is made, and MCP sessions don't offer its tools (the server reads the saved switch for each request).
- Share… lists LocalSend after macOS's services, in place of the LocalSend app's own entry; picking it opens the page holding what was shared. Other apps' share menus would need a Share Extension, which isn't built.
- The sidebar row sits after AirDrop, where Locations' Edit doesn't move it, and shows LocalSend's logo traced as a glyph.

## Goal

Send files to devices running LocalSend and receive files from them, without the LocalSend app. One `monodrive-core` service; the window and MCP both call it.

## Protocol and implementation

- Protocol v2.2, which LocalSend 1.18 speaks. v3 isn't used by the app yet.
- LocalSend's own Rust core (`packages/core`), the one its app and CLI use, pinned by git commit with only the `discovery` feature (`http` and `multicast`). It runs on tokio, so the service keeps one tokio runtime of its own per process; GPUI's executor never runs its futures.
- cargo-deny: allow its git source, allow ISC (ring, untrusted, rustls-webpki), and ignore RUSTSEC-2023-0071 (`rsa` 0.9's timing side channel) with the reason: the key signs one self-signed certificate per Mac and never decrypts. `webrtc` stays off.

## One identity per Mac

`localsend/identity.pem` in the support folder (0600) holds an RSA-2048 key and its self-signed certificate, made on first use. The window and every `monodrive mcp` use it, so peers see one fingerprint. The alias is the Mac's computer name, the model "Monodrive", the type desktop.

## Finding devices without advertising

Looking for devices never makes this Mac a target nobody answers for. Unless it's receiving, Monodrive doesn't announce itself or register with anyone:

- It listens on LocalSend's multicast groups (UDP 53317, `SO_REUSEPORT`, so it shares the port with the LocalSend app) and checks each announcer with a `GET /info` pinned to the fingerprint it claimed.
- It asks every host on each local IPv4 /24 for `GET /info` on 53317, 50 at a time with a 500 ms timeout, and checks each answer the same way.

Since 1.18 the app answers announcements over HTTP only, so without receiving there's nothing to announce to. While receiving, Monodrive also announces and answers announcements, as the app does.

## Sending

The device is named by fingerprint, and every request is pinned to it, so nothing reaches another certificate. Folders go as their files, named by their path from the folder (`Photos/2026/a.jpg`), which the app and the CLI do too. Links and special files are skipped. `prepare-upload` with an optional PIN, then one upload at a time with progress and cancel. 204 means the receiver took nothing, 401 a PIN, 403 declined, 409 busy, 429 too many PIN tries.

## Receiving

Off by default; Settings and the LocalSend page turn it on. Only the window receives: it lives as long as the user wants it, while the ops owner exits when idle and an MCP server ends with its session.

- The server takes 53317, or a port macOS picks when another app (the LocalSend app) holds it; the announcement carries the port.
- Every offer asks in a card in the window: who, how many files and how big, the first names, and where they'll go. ↩ accepts, Esc declines. Nothing auto-accepts.
- A message (one text file whose preview holds the text, as the LocalSend app sends it) shows its text in the card. Copy takes none of its files, which answers 204 as the app does, puts the text on the clipboard and keeps it in `localsend/messages.json` (0600, newest 50), which Received Text on the page lists to copy again, forget or clear. Agents don't read it: a message may be a password.
- An offer plays AirDrop's invite chime and brings the app forward. macOS may keep another app in front, so while one is, the offer also posts a notification whose Decline and Accept (or Copy) answer it, and how a transfer ended posts one too. Only a bundled build can post them; an agent's run neither chimes, comes forward nor posts.
- Optional PIN: Monodrive makes a 6-digit one, shown on the page and in Settings, with New PIN.
- Files go to the save folder, Downloads by default, chosen in Settings. Each file is written under a hidden `.monodrive-localsend-…` name in that folder and renamed into place under a free name (Finder's `name 2`), so a partial file never takes a real name. A folder keeps its structure; each part of a name is sanitized and `..` can't climb out. A failed file goes to the Trash. Received files get `com.apple.quarantine`.
- While a transfer runs, a card where toasts show says who, how far, and Cancel. A toast says how it ended.

## MCP

In the `all` profile, so `core` stays within its budget:

- `localsend_devices`: devices nearby, found as above for up to `wait_seconds`, with their fingerprints.
- `localsend_send`: `fingerprint` (required, from `localsend_devices` in this session), `paths`, optional `pin`. Waits for the receiver to answer and the files to arrive. The description says LocalSend can auto-accept from favorites, so the send may land unconfirmed.

Agents don't receive: an incoming transfer always asks a person in the window. Failures that look like a blocked Local Network say so, since the prompt names whichever app started the agent.

## macOS

`NSLocalNetworkUsageDescription` in the Info.plist `scripts/bundle` writes (release builds go through it too). No entitlement or `NSBonjourServices`. Dev builds are ad hoc signed, so the firewall and Local Network may ask again after a rebuild.

## Window

Following the AirDrop page's layout:

- Locations gets a LocalSend row after the drives, before the servers. It opens the LocalSend page (Go › LocalSend, the palette).
- The page: a header with this Mac's name and whether it's receiving, a grid of devices with their kind's icon to drop files on or click to choose files, Scan Again, and a footer with Receive files, the PIN and the save folder.
- Send with LocalSend on a selection opens the page holding those items; a device takes them. Esc lets go.
- Saved as its folder in a session, so a launch never looks for devices.

## Tests

Nothing touches the LAN, and nothing listens on a wildcard address: the core's server binds `0.0.0.0` and `::`, which raises the firewall's prompt for every new test binary on a Mac with it on.

- Sending runs against a loopback peer that answers as a receiver would: the files and names that arrive, a folder's names, decline, PIN, busy.
- Receiving drives the service through the core's event channel with the events its server sends, writing what the server would: names, conflicts, folders, `..`, quarantine, decline, a failed file.
- Identity: made once, the same fingerprint after.
- MCP: the contract, and `localsend_send` refusing a fingerprint this session didn't list.
