# LocalSend

LocalSend is opt-in: Settings › General's Use LocalSend, off by default. While it's off, nothing of it shows (sidebar, menus, palette, Share…), nothing listens or looks, no key is made, and agents' sessions don't offer its tools: the MCP server reads the saved switch again for each tool list and call, so turning it off reaches a session already running.

Monodrive speaks LocalSend's protocol (v2.2) through LocalSend's own Rust core, pinned by commit. `monodrive_core::localsend` is the one service; the window and the `localsend_devices` and `localsend_send` tools call it.

## One identity per Mac

`localsend/identity.json` in the support folder (0600) holds the key and self-signed certificate, made on first use. The window and every MCP server share it, so peers see one device whose fingerprint never changes.

## Looking isn't announcing

Finding devices never makes this Mac a target. Unless it's receiving, Monodrive hears announcements on LocalSend's multicast port (shared with the LocalSend app through `SO_REUSEPORT`) and asks each Wi-Fi and Ethernet /24 for `GET /info` on 53317. It doesn't announce or register, since peers would then list a Mac nobody answers for. Every device found is checked with a request pinned to the fingerprint it claimed, and every send is pinned too, so a claim alone reaches nothing.

## Only the window receives

Receiving needs a process that lives as long as the person wants: the ops owner exits when idle and an MCP server ends with its session. So the window runs the server, while Settings' Receive files is on, on 53317 or a port macOS picks when another app has it. Each offer asks a person; agents never accept. Files arrive under a hidden name in the save folder and take a free name once whole, with `com.apple.quarantine`.

## macOS

Local Network access belongs to the app that's responsible for the process. For `monodrive mcp` that's whatever started the agent, which may refuse it, so failures that look like it say where to turn it on. The core's server listens on every address, which a Mac's firewall asks about, so tests drive the receiving side through the server's events instead of a socket.
