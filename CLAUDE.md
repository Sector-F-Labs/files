# Files

A Patina (egui) file manager whose point is that `Name.app` folders are apps (Patina OS's Mac
model). Read the ADRs in `docs/adr/` before changing how apps, operations or the clipboard work;
use the `adr` skill to add one.

## Commands

- `make check` (tests + clippy -D warnings) before committing.
- `make install` installs per user; test with `PREFIX=<scratch dir>`.
- `cargo run` opens a window on the current desktop; the user can try it there. For driving it
  yourself, a headless sway needs a virtual pointer that stays connected (wlrctl's goes away
  after each command, so clicks never arrive).

## Layout

- `src/app.rs`: the window: menus and tabs, toolbar and path bar, sidebar, icon and list views,
  keys, drag and drop, the selection band, rename field, dialogs. Widgets push `Action`s; `apply`
  does them after the frame.
- `src/listing.rs`: reading a folder or the Trash on a thread, `Entry`, sorting.
- `src/ops.rs`: copy, move, trash, restore and purge on one worker thread with progress and
  Stop; free names ("name copy"), new folder, rename.
- `src/icons.rs`: theme icons, app icons and thumbnails on a thread, newest request first.
- `src/mime.rs`: types from shared-mime-info's `globs2`, icon names, descriptions.
- `src/places.rs`, `src/watch.rs` (notify), `src/uri.rs` (file URIs, the clipboard's form),
  `src/config.rs` (`~/.config/files/config.toml`), `src/glyphs.rs` (painted toolbar symbols).

## Rules

- Paint colours only from `ui.visuals()`, so skins reach them.
- Never overwrite a file: operations take a free name.
- Anything that reads the disk runs off the UI thread.
