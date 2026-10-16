# 0002. A .app folder is one item, opened by running it, read by patina-apps

Date: 2026-10-16

## Status

Accepted

## Context

On a Mac an app bundle is shown as one file wherever it is, with its own icon and without the
`.app`, and opening it runs it; "Show Package Contents" goes inside. Patina OS's format 1
(its ADR-0004) says how a folder app runs. The desktop's launcher already read those rules;
copying them here would let the two drift apart.

## Decision

- The rules live in the patina-apps crate (its own private repository), which Files and the
  desktop's launcher both depend on by git.
- Any folder named `*.app` (not only those in `~/Apps`) is shown as one item named without
  `.app`, with the icon its `app.toml` names, and runs when opened; right-click → Show Contents
  goes inside. One that can't run here (another platform, a format it doesn't know) says why
  in the status bar.
- Renaming keeps the `.app`; copies are "Name copy.app".

## Consequences

- Downloading an app and double-clicking it works before it's installed, as on a Mac; installing
  is dragging it to Apps.
- Running an app from anywhere is as safe as running any program the user has; there's no
  quarantine or signature check.
