# Changelog

All notable changes to Toranj. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Fixed
- New terminals didn't start their command (e.g. `claude`) and showed "no such session" (regression in 0.1.0).

### Changed
- On launch, Toranj opens the terminal you last had on screen, or the Overview, instead of the first project by name.

### Added
- Groups stand out: in the sidebar a group is one tinted box, and on the Overview a group's project cards sit together under its name (click it to see all its terminals).
- Close a project's terminals (or a whole group's) from its right-click menu.
- New terminal can open a whole workspace: a folder holding several projects (e.g. an app, its backend and website) is offered as "<folder> · whole workspace · N projects", so the agent gets all of them as context. In the sidebar it joins the folder's group as "All projects".

## [0.1.0] - 2026-11-17

First public release: one window for all your coding agents, with statuses, notifications, the waiting dock, workspaces and grids, drag to group and reorder, project icons, a Claude Code status line, and signed, notarized builds.

[Unreleased]: https://github.com/hrvojecukman/toranj/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/hrvojecukman/toranj/releases/tag/v0.1.0
