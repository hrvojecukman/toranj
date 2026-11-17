# Toranj architecture

This document describes how Toranj works today. For what it does and what it changes on your machine, see the [README](../README.md).

## Processes

```
┌──────────────────────── Toranj.app ────────────────────────┐
│  WebView (src/main.ts, xterm.js)                           │
│     │ Tauri IPC (invoke / channels / events)               │
│  Rust backend (src-tauri/src)                              │
│     ├─ pty.rs: one `tmux attach` client per visible pane   │
│     ├─ status.rs: snapshot every 2 s                       │
│     └─ spool watcher (notify crate)                        │
└─────┬──────────────────────────────┬───────────────────────┘
      │ tmux -L toranj               │ reads ~/.local/state/toranj/events
┌─────▼───────────────┐      ┌───────┴─────────────────────┐
│ tmux server         │      │ toranj-hook                 │
│ one session per tab │      │ run by Claude Code hooks    │
│ shell → agent / CLI │      │ (and as its status line)    │
└─────────────────────┘      └─────────────────────────────┘
```

- **Tauri app.** The Rust backend runs commands and owns all file and process access. The frontend is plain TypeScript with no framework (`src/main.ts`, `src/notify.ts`, `index.html`, `src/styles.css`). xterm.js renders the terminals, with the WebGL renderer when it is available.
- **tmux server.** Every Toranj tab is a tmux session on a private socket (`tmux -L toranj`). The config is written to `~/Library/Application Support/toranj/tmux.conf` and re-sourced at startup. tmux keeps the agents alive when the window closes or the app crashes. When the window is destroyed, only the attach clients are killed.
- **toranj-hook** (`crates/toranj-hook`). A tiny binary with no GUI dependencies that Claude Code runs for each hook event. It reads the event JSON on stdin, appends one record to the spool, and always exits 0 without printing anything, so it can never break Claude Code. Run as `toranj-hook statusline`, it prints the project label for Claude Code's status line.
- **toranj-spool** (`crates/toranj-spool`). Code shared by the hook (the writer) and the app (the reader): the record format, paths, ID validation, redaction, append, and tail-reading.

## Tabs and terminals

`tmux::create` starts a session named `<project-slug>__<command-slug>[-n]` in the project folder. The session runs the user's login shell and gets `TORANJ_TAB=<name>` in its environment. The command is then typed into that shell, so it gets the user's full `PATH` and leaves a shell behind when it exits. Commands with control characters are refused. The session stores its project path in the tmux option `@toranj_project`. Sessions Toranj did not create have no project, and Toranj never guesses one from the cwd.

`pty.rs` connects a visible pane to its session by running `tmux attach-session` in a PTY (portable-pty). Output goes to the frontend over a Tauri channel, and keystrokes and resizes come back over `write`/`resize`. Each pane has its own attach client, so a grid of four panes uses four clients.

`tmux::list` reads every pane in one `list-panes -a` call. Each line gives the session name, project, cwd, pane PID, current command, activity time, a bell flag, and `@toranj_attached` (the session ID of a background Claude that the tab is attached to). The bell flag comes from an `alert-bell` hook that sets `@toranj_bell`.

## Status

The frontend calls `snapshot` every 2 seconds, and also right after a spool change (see below). `status::snapshot` builds the snapshot from:

1. `ps -axo pid,ppid,comm`: the process tree.
2. `claude agents --json`: one row per Claude session, with its PID, cwd, status, `waitingFor`, session ID, kind (`interactive` or `background`) and short ID.
3. The tmux session list.

Each tab is matched to its status this way:

- **Claude adapter.** A tab speaks for a `claude agents` row when any of these is true:
  - the row's PID descends from the tab's pane PID;
  - the tab is attached to that background session (`@toranj_attached`);
  - a Claude in the pane *parked* its conversation in a background job. Claude Code writes `parkedJobId` to `~/.claude/sessions/<pid>.json`. That file is only trusted while the process start time still matches.

  The row's status maps to `running`, `waiting`, `error`, `done` or `idle`.
- **Hook refinement.** When the hooks are on, `spool::last(session_id)` reads the newest record (only the last 16 KB of the file):
  - A `PermissionRequest`, `Elicitation` or permission-type `Notification` less than 10 s old turns `running` into `waiting`. This bridges the gap before `claude agents` notices the prompt.
  - `StopFailure` turns `idle` into `error`.
  - `Stop` with background tasks still running keeps the tab `running` for up to 10 minutes.
  - The hook's short `detail` fills in what the agent is waiting for.
- **Generic adapter** (every other tab). The agent is the first known agent binary in the pane's process tree (`claude`, `codex`, `gemini`, `aider`, `opencode`, `cursor-agent`, `amp`). If there is none, it is the pane's command, or `shell`. The state is:
  - `waiting` if the terminal rang the bell;
  - `idle` for a bare shell;
  - `running` if there was output in the last 3 s;
  - `idle` for a quiet agent;
  - otherwise `running` (for example, a dev server with no output).

The snapshot also carries `external`, the Claude sessions not shown in any tab (see below), and `git`: branch, changed files, ahead and behind for each project that has open tabs. `git.rs` caches git results for 8 s and kills a git call after 3 s. It skips `git status` in repos with repo-local clean/smudge filters, because those filters would run code from the repo.

### Spool and hooks

`~/.local/state/toranj/events/` (or `$XDG_STATE_HOME/toranj/events`) holds one append-only JSONL file per Claude session ID. The directory is created as `0700` and refused if it is a symlink or owned by someone else. Files are opened `0600` with `O_NOFOLLOW`. Each record holds the timestamp, event, session ID, tool name, a kind (notification type, SessionStart source or SessionEnd reason), the number of background tasks, the `TORANJ_TAB` value, and a short redacted `detail`, such as the first line of a Bash command or a file path. Tool output and file contents are never stored. Files untouched for 7 days are deleted when the app starts.

The app watches the directory and emits `toranj://spool`. Bursts of events are coalesced: 150 ms in Rust, then at most one refresh every 500 ms in the frontend.

`claude_hooks.rs` installs and removes the hooks:

- It copies the bundled sidecar to `~/.local/share/toranj/bin/toranj-hook`, so `settings.json` never points into an app bundle that may move.
- It adds one entry per event to `~/.claude/settings.json`. Tool events are marked `async`.
- It only ever touches entries whose command is `toranj-hook`.
- It writes atomically and keeps the file's permissions.
- It follows a symlinked `settings.json` to the real file, but writes backups to `~/.claude` (the last three are kept).
- If Claude Code changes the file during an edit, the edit is redone.

The status line option works the same way and refuses to replace a status line that is not Toranj's.

### Status line labels

The frontend calls `set_status_labels` with `{tabs: {name: {label, color}}, projects: {path: {...}}}`, and the backend writes it to `~/.local/state/toranj/labels.json`. `toranj-hook statusline` looks up its `TORANJ_TAB`. If the tab is not listed, it uses the longest project path that contains Claude's cwd. It then prints the label in the project's color. Outside Toranj projects it prints nothing.

## Titles

`titles.rs` reads the conversation title from Claude's transcript, `~/.claude/projects/<cwd with non-alphanumerics replaced by '-'>/<session>.jsonl`. It looks for three kinds of records: `custom-title` (`/rename`), `agent-name` (background sessions) and `ai-title`. Transcripts can grow to tens of MB, so each one is streamed once, and after that only the bytes appended since the last read are read. Long lines are only checked at their start. Titles are cleaned (one line, no control characters, at most 80 characters) and forgotten when a session leaves `claude agents`.

## Notifications and attention

The frontend compares each snapshot with the previous one:

- **Done or error.** A Claude tab that goes from `running` to anything else, while you are not looking at it, is marked `done` or `error` until you look. This sends a notification and plays the "Finished" sound. Its git cache is also invalidated.
- **Waiting.** A tab that starts waiting, while you are not looking at it, sends a notification and plays the "Waiting for you" sound.

Notifications go through `notifications.rs`, which uses the native UserNotifications framework so they come from Toranj itself. Clicking one emits `notification-click` with the tab name. If native notifications are unavailable, the Tauri notification plugin is used instead. Sounds are played by the app with `afplay` and a macOS system sound, because macOS mutes notification sounds while the app is in front. The Dock badge counts tabs that need you.

The waiting dock calls `peek_tab`, which returns the last visible lines of the tab plus any numbered select prompt at the bottom of the screen. A prompt is a contiguous list starting at 1 with the `❯` cursor on one entry, and it gets a fingerprint. `answer_tab` re-reads the screen and sends the digit only if the same prompt (same fingerprint) is still there. ⌘U cycles through waiting tabs first, then finished tabs you have not seen yet.

## Grouping, layouts and order

The sidebar groups tabs by project: the tab's `@toranj_project`, or its cwd. Projects can be grouped further into **workspaces** (`workspacesFor` in `main.ts`). For each project, in this order:

1. A manual assignment in `org.ws`: a dragged group `m:<id>`, or `-` for "not in a workspace".
2. A VS Code `*.code-workspace` file that lists the project's folder. `workspaces.rs` looks for these files in each project's folder, its parent and its grandparent (never `~` or above) and parses them as JSONC.
3. A shared parent folder, unless it is a generic name such as `Projects`, `Developer`, `code` or `src`.

An automatic workspace needs at least two projects. All of this state lives in `localStorage`:

- `toranj.org`: display names, workspace assignments and names, collapsed workspaces, and the sidebar order (`w:<id>` and `p:<path>` keys).
- `toranj.views`: the layout for each project or `ws:<id>` (`single`, `grid`, `cols` or `rows`; workspaces default to `grid`) and the pane order.
- `toranj.settings`: font size, theme and sounds.

⌘1–9, the grid and the overview all follow the sidebar order.

## Outside sessions

Any `claude agents` row that no tab claims becomes an **external** session, shown under the project whose path contains its cwd. `unlisted_claudes` also finds interactive `claude` processes that `claude agents` does not list. It uses `ps`, and `lsof` for the cwd, cached per PID. These sessions have no known session ID, so they can only be moved by hand.

`move_external(session_id, force)`:

- **Background session** (`claude --bg`). Creates a tab that runs `claude attach <short id>` in the session's folder and sets `@toranj_attached`. Nothing is stopped. If the folder was moved or deleted, it refuses and explains how to fix it. When you close such a tab, you are asked whether to stop the background session too. That runs `claude stop`, or `claude rm` for a listed session whose process has exited.
- **Interactive session.**
  - It must be idle, or `force` must be set because you confirmed interrupting it.
  - Its PID must still belong to that Claude (checked by command and start time).
  - Its transcript must exist.

  It is then sent SIGTERM. Once it has exited, a new tab runs `claude --resume <id>` in the same folder.
- A per-ID guard stops a double click from starting two copies.

## Other panels

- **Git panel** (`git::details`): changed files, the last 20 commits with pushed or unpushed marks, branches (merged, gone upstream, unpushed), worktrees and stashes. All read-only; Toranj never fetches.
- **Memory panel** (`system.rs`): RAM and swap, and the processes grouped into Docker/VM, VS Code, iOS simulator, Android emulator, Gradle/Kotlin, AI agents, Browsers and Language servers. It measures memory by physical footprint, not RSS. `stop_process` only signals a PID that a recent read offered as stoppable, and checks its start time and executable first.
- **Project icons** (`icons.rs`): looks through a list of usual icon paths (app icons first, then favicons) in the project folder and one level down. A project like `shop-backend` can also use the icon of a sibling folder named after its first word (`shop`).
- **Project list** (`projects.rs`): git repositories up to 4 levels deep under `~/Developer`, `~/Projects`, `~/code`, `~/src`, `~/dev` and `~/Documents/Projects`.

## Repository layout

```
src/                  frontend
  main.ts             UI: sidebar, panes, overview, dock, palette, panels, settings
  notify.ts           notifications, sounds, Dock badge
  styles.css
  dev/mock.ts         ?mock: renders the UI in a browser with sample data
  dev/demo-data.json  fictional sample data (?mock&demo), used for screenshots
index.html
src-tauri/src/        backend
  lib.rs              commands, menu, spool watcher, logging
  tmux.rs pty.rs      tmux sessions and attach clients
  status.rs           snapshot, adapters, outside sessions, moving them in
  titles.rs           conversation titles from transcripts
  claude_hooks.rs     hooks and status line in ~/.claude/settings.json
  notifications.rs    native macOS notifications
  git.rs              git summary and details
  system.rs           memory panel
  workspaces.rs icons.rs projects.rs
crates/toranj-hook    hook and status line binary (bundled as a sidecar)
crates/toranj-spool   spool format shared by the hook and the app
scripts/build-hook.sh builds the sidecar into src-tauri/binaries/
scripts/dump-mock.sh  dumps live data to src/dev/mock-data.json (gitignored)
```

To work on the UI without Tauri, run `npx vite` and open `http://localhost:1420/?mock&demo`. Parameters such as `ov`, `wait`, `done`, `ws&wsgrid`, `pal=<query>`, `settings` and `git` set up specific states (see `src/dev/mock.ts`).
