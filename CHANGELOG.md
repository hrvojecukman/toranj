# Changelog

All notable changes to Toranj. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Recently closed: Claude conversations whose terminal closed (or whose Claude exited) are listed in the sidebar with their project icon and title, and in ⌘K. Click one to continue it with `claude --resume`; if it is still open somewhere, Toranj switches to it or offers to move it in instead.
- ⌘1–9 opens the groups and projects in sidebar order (each header shows its number); terminals moved to ⌃1–9.
- The back/forward keys are configurable (Settings → Keyboard): ⌘[ ⌘], ⌘← ⌘→, ⌥⌘← ⌥⌘→ or ⌃⌘← ⌃⌘→.
- Back and forward: ⌘[ returns to the terminal (or overview) you came from, after a dock card, ⌘U, the sidebar or a click on another pane; ⌘] goes forward again. On a Croatian keyboard these are ⌘Š and ⌘Đ. The mouse's back/forward buttons work too.
- Quick open: when a card comes up in the dock, a bar counts down (10 s by default) and one press of Space opens that terminal. While it counts, 1–9 answers a waiting question right from the card, and the card is bigger; afterwards it shrinks back and stays, and ⌥1–9 still answers. The time (or off) and the key are in Settings. The newest card (waiting or finished) comes to the front for its countdown, which always runs its full time; a key pressed within a second of other typing goes to the terminal as usual.
- Use Toranj from your phone (Settings → Phone). A web app you add to your Home Screen shows every terminal's status, answers Claude's questions with a tap, shows a terminal and types into it, opens new terminals (including continuing a conversation), and sends push notifications when an agent finishes, waits for you or stops with an error (optionally only when nobody is at the Mac). It is reached through Tailscale: only devices signed in with the Mac owner's Tailscale account get through, never from the internet, and each phone pairs once with a single-use code. Notifications are encrypted for the phone and sent straight from the Mac.
- New terminal can continue a Claude conversation: with `claude` chosen, it lists the folder's recent conversations (title and when), and picking one runs `claude --resume <id>`. Conversations open in a running Claude are left out.

### Fixed
- Only one Toranj runs at a time: opening it again (another copy, a notification click) brings the open window to the front instead of starting a second copy that showed no groups and every terminal as outside.
- Clicking into a terminal could copy a word or a stray character and replace your clipboard ("Copied"). Only a real mouse selection copies now; copies a program makes on its own (e.g. Claude's /copy) still work.
- Dark project icons on transparency (a black logo) were invisible in the dark sidebar; they now get a light backdrop. Icons that aren't square are no longer cropped.
- A question answered from the dock or the phone no longer leaves the terminal "waiting" (and its card up) while Claude works on the approved step; it shows as running until a new question comes.
- Answering from the dock failed with "the question changed" on Claude prompts that count down ("…automatically block this request in 1:48").
- ⌘-click on a link in a terminal opens it in your browser again (plain URLs and the links Claude prints).
- Text couldn't be selected in a shell after Claude (or another program) exited without turning mouse reporting off. Toranj now turns it off once the pane is back at a shell prompt.
- Selecting text shows a short "Copied" in the pane, since a mouse selection goes straight to the clipboard.

### Changed
- New terminal lists workspaces (VS Code workspaces and whole folders) and projects in two labelled sections, with each workspace's folders on one line.

## [0.1.1] - 2026-11-17

### Fixed
- New terminals didn't start their command (e.g. `claude`) and showed "no such session" (regression in 0.1.0).

### Changed
- On launch, Toranj opens the terminal you last had on screen, or the Overview, instead of the first project by name.

### Added
- New terminal lists your VS Code workspaces (`*.code-workspace` next to your projects): it opens in the workspace's first folder and starts `claude` with `--add-dir` for the others, so the agent sees exactly the workspace's folders. The folder entry is now labelled "whole folder".
- Resize panes: drag the line between two terminals (double-click it to even them out). Each row sizes its own panes, and rows their heights. An even split snaps into place, with a tap on a Force Touch trackpad. Sizes are kept per project or group.
- Groups stand out: in the sidebar a group is one tinted box, and on the Overview a group's project cards sit together under its name (click anywhere in the box to see all its terminals). Clicking a project in the sidebar opens it (inside a group, in the group's grid).
- Close a project's terminals (or a whole group's) from its right-click menu.
- New terminal can open a whole workspace: a folder holding several projects (e.g. an app, its backend and website) is offered as "<folder> · whole folder · N projects", so the agent gets all of them as context. In the sidebar it joins the folder's group as "All projects".

## [0.1.0] - 2026-11-17

First public release: one window for all your coding agents, with statuses, notifications, the waiting dock, workspaces and grids, drag to group and reorder, project icons, a Claude Code status line, and signed, notarized builds.

[Unreleased]: https://github.com/hrvojecukman/toranj/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/hrvojecukman/toranj/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/hrvojecukman/toranj/releases/tag/v0.1.0
