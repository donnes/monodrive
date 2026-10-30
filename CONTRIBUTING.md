# Contributing

## Read this first

Opening a PR does not oblige anyone to review or merge it. We may close or defer it, ask for a smaller scope, or reimplement the idea later.

Focused bug fixes, reliability fixes and performance improvements are the most likely to be accepted. Unsolicited features, rewrites and unrelated cleanup are not.

Report bugs and propose features in [issues](https://github.com/donnes/monodrive/issues). Search the existing issues before you start work.

## Get it running

You need macOS 14 or newer, [stable Rust](https://rustup.rs) and the Xcode Command Line Tools. `scripts/bundle` also needs Xcode 26 or newer, and `scripts/dev` needs [mise](https://mise.jdx.dev) to install its watcher.

1. Fork the repository and clone your fork.

   ```sh
   git clone git@github.com:<you>/monodrive.git
   cd monodrive
   ```

2. Run the checks. They build everything and open no window, so they don't start the index.

   ```sh
   scripts/check
   ```

3. Open the app on a folder, or draw every surface without opening a window.

   ```sh
   cargo run -p monodrive -- ~/Work
   cargo run -p monodrive --example snapshot
   ```

Opening the window starts the index. On a Mac without one, that crawls the whole disk, which takes a few minutes and several hundred MB of disk and memory. On a Mac with one, your build can become the process that owns the index, so quit it when you're done. Never delete or edit `~/Library/Application Support/FSearch`.

## Where things live

- `crates/monodrive-core` holds the services and has no GPUI.
- `crates/monodrive` is the GPUI app.
- `vendor/` is a patched GPUI. Don't edit it.

A new capability is a `monodrive-core` function that both the view and the MCP tool call. [AGENTS.md](AGENTS.md) lists the files in each crate and the rules for working here, whether you or an agent writes the code. Read [DESIGN.md](DESIGN.md) before any UI work.

## Send a PR

1. Open or find an issue, unless your change is a small fix for an obvious bug. Features need a maintainer's approval first.
2. Create a branch and make one change.
3. Run `scripts/check`.
4. Open a PR against `main` and fill in the [template](.github/pull_request_template.md). Use a conventional commit title in plain language, such as `fix(browser): keep the selection after a rename`.

The sections below explain what a maintainer looks for.

## Establish the problem and scope first

Features and intentional changes to behavior need a maintainer's explicit approval of the direction and scope. Link that approval in your PR. A linked issue alone is not approval, and calling a behavior change a bug fix does not exempt it.

For a substantial bug fix, link an issue that describes the failure and the intended behavior.

A small fix for an obvious bug needs no prior issue. Explain the defect and why the fix qualifies. We judge its purpose and effect, not its line count.

## Solve one problem per PR

A PR that solves several independent problems must be split, even when each fix is useful. Include the core, MCP, view and test changes that one problem needs. An adjacent cleanup or a second fix gets its own PR.

## Show that it works

Explain how you established the problem, how you checked the change and what you observed. State what you could not check. A checkbox or "tests pass" does not show that the change works.

Run `scripts/check` before you push, since CI runs the same checks. [AGENTS.md](AGENTS.md#verifying) says which behavior gets a test and how to run the benchmarks.

UI changes need before and after images, and `cargo run -p monodrive --example snapshot` draws them. Motion needs a short video. Attach evidence to the PR and never commit it.

We assess the problem, scope, approval and evidence, not whether an agent wrote the PR.

## Merging

Every change to `main` from a contributor goes through a PR. A PR can merge once the `macOS` and `Dependencies and licenses` checks pass, and we squash or rebase it. A maintainer approves CI for a first-time contributor's first PR.

## Size labels

Every PR also gets a `size:*` label from `size:XS` to `size:XXL`, counted from its changed lines. The count leaves out `vendor/`, `Cargo.lock` and test files, unless the PR only changes tests. The label is context for reviewers, not a limit.

## Vouching

The vouch workflow labels every PR by its author's status in [.github/VOUCHED.td](.github/VOUCHED.td):

- `vouch:trusted` means the author is a collaborator, a bot, or listed in the file.
- `vouch:unvouched` means the author is not listed yet. This is where everyone starts.
- `vouch:denounced` means the file blocks the author, and a maintainer closes the PR.

The trusted and unvouched labels tell reviewers who they are dealing with. Neither one decides whether a PR is eligible or merges, and a vouched author's PR meets the same requirements as anyone else's.

Maintainers add people to the file after good contributions. Don't send a PR that vouches for yourself. The workflow reads the file from `main`, so changing it in your PR has no effect on your own label. When the file changes on `main`, the workflow relabels every open PR. Comment `/recheck-vouch` on a PR to relabel just that one.

## Conduct and security

Be kind: see [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Report vulnerabilities privately: see [SECURITY.md](SECURITY.md).
