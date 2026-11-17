# Contributing to Toranj

Thanks for taking a look. Bug reports, ideas and pull requests are all welcome.

## Before you start

- For anything larger than a small fix, open an issue first so we can agree on the approach.
- Toranj is macOS-only for now.
- Much of what Toranj knows about Claude Code comes from parts that aren't a public API (`claude agents --json`, `~/.claude/sessions/<pid>.json`, transcript files). When a Claude Code update breaks something, please mention the Claude Code version (`claude --version`).

## Setup

You need macOS, [Rust](https://rustup.rs) (stable), Node.js 22+ and tmux (`brew install tmux`).

```sh
npm install
sh scripts/build-hook.sh   # builds the Claude Code hook sidecar
npm run tauri dev          # runs the app with hot reload
```

## Checks

Please run these before opening a pull request (CI runs them too):

```sh
npm run build                            # type-checks and builds dist/, which the app crate embeds
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

The UI can be rendered without the Rust backend for screenshots: start `npx vite` and open `http://localhost:1420/?mock&demo` (see `src/dev/mock.ts` for the other parameters).

## Style

- UI text is in English and short. Say what happened and what to do next.
- Keep the app light: no new runtime dependencies without a good reason.
- Comments explain *why*, not *what*.
- Never send anything over the network, and never write outside the paths listed in the README without asking the user first.

## License

By contributing you agree that your contributions are licensed under the MIT License.
