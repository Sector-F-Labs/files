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

## Testing the portal

The chooser can be driven without touching the desktop's own portal: a private bus with
`dbus-run-session`, `XDG_DESKTOP_PORTAL_DIR` pointing at a folder holding `patina.portal` *and*
`patina-portals.conf` (with that variable set, the portal reads its config only from there),
`XDG_CURRENT_DESKTOP=Patina`, the D-Bus service file in `$XDG_DATA_DIRS/dbus-1/services`, and a
client that stays connected until `Response` arrives (the portal drops a request whose caller
has gone, so `gdbus call` alone doesn't work; Python's Gio does). Show it on a headless sway and
type with one `wtype -s 1000 ...` run (separate runs add and remove the keyboard, and keys get
lost). `pgrep -f` matches your own shell: find processes with `pgrep -x files`.

## Layout

- `src/app.rs`: the window: menus and tabs, toolbar and path bar, sidebar, icon, list and column
  views (the tab's folder is the column with the keys; the others are `column_lists`),
  keys, drag and drop, the selection band, rename field, dialogs. Widgets push `Action`s; `apply`
  does them after the frame.
- `src/listing.rs`: reading a folder or the Trash on a thread, `Entry`, sorting.
- `src/ops.rs`: copy, move, trash, restore and purge on one worker thread with progress and
  Stop; a taken name waits on the window's answer (merge, replace, keep both, skip; ADR-0009);
  free names ("name copy"), new folder, rename.
- `src/icons.rs`: theme icons, app icons and thumbnails on a thread, newest request first.
- `src/preview.rs`: Quick Look's pictures (ADR-0008), decoded at screen size on a thread, newest
  only, EXIF-turned (`open`, which thumbnails use too). Its window is an immediate viewport,
  `Files::quick_look`, which runs `keys` on its own input.
- `src/properties.rs`: the Properties window's facts (stat, owner and group, free space, folder
  contents counted until the window closes) and permission changes, on threads; drawn by
  `Files::properties_window`.
- `src/mime.rs`: types from shared-mime-info's `globs2`, icon names, descriptions.
- `src/editor.rs`: the text editor for a script's Edit (ADR-0011), from the mimeapps lists.
- `src/portal.rs`: `files --portal`, the `org.freedesktop.impl.portal.FileChooser` service;
  each request runs `files --choose` (a process per dialog: one winit loop per process).
  `src/chooser.rs`: the request and answer (TOML over stdin/stdout), filters and globs.
  Picking mode in `app.rs` is `Files::picking`: no menus or tabs, the chooser bar, `accept`.
- `src/dnd.rs`: drags as Wayland drags (ADR-0006, ADR-0007), on a second connection onto winit's
  display with its own queue and thread. Other apps' file drags are taken too, their list read
  as they come over; `Over` events give egui any drag over the window as pointer events and a
  payload, so drops stay egui's.
- `src/zfs.rs`: the status bar's ZFS facts for the folder shown: the dataset from `/proc/self/mountinfo`, the numbers from `zfs get`, on a thread.
- `src/places.rs`, `src/watch.rs` (notify), `src/uri.rs` (file URIs, the clipboard's form),
  `src/config.rs` (`~/.config/files/config.toml`), `src/glyphs.rs` (painted toolbar symbols).

## Rules

- Paint colours only from `ui.visuals()`, so skins reach them.
- Never overwrite a file: operations take a free name, or put what they replace in the Trash.
- Anything that reads the disk runs off the UI thread.
