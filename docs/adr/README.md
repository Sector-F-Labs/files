# Architecture Decision Records

This directory holds ADRs for Files — short records of significant architecture decisions,
written at the time they're made, following Michael Nygard's format as popularized by Martin
Fowler: <https://martinfowler.com/bliki/ArchitectureDecisionRecord.html>.

## When to write one

Write an ADR when a decision is:

- Hard, or expensive, to reverse once other code depends on it.
- Not obvious from reading the code — a "why", not a "what".
- The kind of thing that'll get re-litigated later without a record ("didn't we already decide
  this?").

Small, reversible choices don't need one. Rule of thumb: if you'd want to explain the choice to a
new contributor in a paragraph beyond what the code itself shows, it's ADR-sized.

An ADR is a point-in-time record of *a decision as it was made*, including the options considered
and rejected. It does not get rewritten later — a decision that reverses or replaces an earlier one
gets its own new ADR that supersedes the old one, which stays in place with its status updated.

## Convention

- One file per decision: `NNNN-short-kebab-title.md`, numbered sequentially, zero-padded to 4
  digits. Numbers are never reused, even for a decision that gets reversed.
- Start from [`0000-template.md`](0000-template.md) — copy it, don't write from scratch.
- `Status` is one of: `Proposed`, `Accepted`, `Rejected`, `Deprecated`, or `Superseded by
  ADR-NNNN` (linked to the superseding record).
- Every new ADR gets a row in the index below.
- Keep records short — a paragraph or two per section is normal. An ADR documents a decision and
  its reasoning, not a design document.

Files is a [Patina](https://github.com/Sector-F-Labs/patina) app. Decisions about the shared
frame, skins and components are recorded in Patina's own ADRs; record here only what this app
decides.

If you're working with Claude Code in this repo, `.claude/skills/adr/SKILL.md` automates the
mechanics of adding one.

## Index

| # | Title | Status |
|---|-------|--------|
| [0001](0001-a-patina-file-manager.md) | Write our own file manager on Patina, because Nautilus can't open .app folders | Accepted |
| [0002](0002-app-folders-are-one-item.md) | A .app folder is one item, opened by running it, read by patina-apps | Accepted |
| [0003](0003-operations-never-overwrite.md) | One operations thread, and a name that's taken is never overwritten | Superseded in part by [0009](0009-taken-names-are-asked-about.md) |
| [0004](0004-the-portal-file-chooser.md) | Files is the Patina session's file chooser, through the desktop portal | Accepted |
| [0005](0005-network-shares-through-gvfs.md) | Network shares through GVfs, as Nautilus has them, opened as folders under its FUSE bridge | Accepted |
| [0006](0006-drags-leave-the-window.md) | Dragging files is a Wayland drag, started on a connection of Files' own | Accepted |
| [0007](0007-drops-from-other-apps.md) | Files takes other apps' drags of files, so files can be dragged between Files windows | Accepted |
| [0008](0008-quick-look.md) | Quick Look: Space shows the selected item in a window of its own | Accepted |
| [0009](0009-taken-names-are-asked-about.md) | A name that's taken is asked about, as Nautilus does: folders merge, a replaced item goes to the Trash | Accepted |
| [0010](0010-the-column-view.md) | The column view: the tab's folder is the column with the keys, the others are read beside it | Accepted |
| [0011](0011-scripts-run-and-edit.md) | An executable script runs when opened, and Edit opens it in the text editor | Accepted |
