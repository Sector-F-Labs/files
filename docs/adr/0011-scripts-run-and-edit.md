# 0011. An executable script runs when opened, and Edit opens it in the text editor

Date: 2026-10-24

## Status

Accepted. The user asked for an executable script to run when opened, with a way to edit it in
the text editor. How a script is told apart and how the editor is found are my design.

## Context

Opening a file handed it to `xdg-open`, which picks the app for its type: for a shell script,
whatever `application/x-shellscript` maps to, often nothing useful, never the script itself. A
script marked executable (Properties' "Allow executing file as program") is meant to be run.

The executable bit alone doesn't say so: FAT and NTFS drives are often mounted with every file
executable, and running a photo, or a binary someone dropped beside it, on a double-click is
the wrong surprise.

## Decision

- **A script is an executable file starting `#!`.** The listing reads the first two bytes of each
  executable file, on its thread (`Entry::script`). Other executables open as before.
- **Opening a script runs it**, in its own folder, detached, as Files starts a terminal. Its
  context menu says **Run** in place of Open, and adds **Edit**.
- **Edit opens it in the desktop's app for `text/plain`,** found in the mimeapps lists in the
  XDG order (the Patina desktop's `patina-mimeapps.list` first), its desktop entry's `Exec`
  filled in, in a terminal if it asks for one; else `$VISUAL` or `$EDITOR` in a terminal
  (`editor.rs`). `xdg-open` can't be told which type to open a file as.

## Consequences

- A script runs without a terminal, so its output isn't seen; one that needs a terminal runs from
  one (Open Terminal Here).
- An executable text file without `#!` opens in its type's app, not run.

## Revisit when

- Scripts that print should be run in a terminal, or asked about first, as Nautilus offers.
- Another Patina app needs "open as text"; it would move into `patina::open`.
