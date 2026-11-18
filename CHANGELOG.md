# Changelog

All notable changes to Toranj. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- New terminal can continue a Claude conversation: with `claude` chosen, it lists the folder's recent conversations (title and when), and picking one runs `claude --resume <id>`. Conversations open in a running Claude are left out.

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
