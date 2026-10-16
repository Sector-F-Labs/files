# Files

A [Patina](https://github.com/Sector-F-Labs/patina) file manager, where a `Name.app` folder is an
app, as on a Mac: shown as one item with its own icon, and run when it's opened (right-click →
Show Contents to look inside). Apps are folders with an `app.toml` (Patina OS ADR-0004); the
rules come from the shared [patina-apps](https://github.com/Sector-F-Labs/patina-apps) crate, which the Patina desktop's
launcher uses too. It runs on any Linux desktop (and FreeBSD), so `.app` folders work wherever
Files is installed.

- Folders as icons or a list (Ctrl+1, Ctrl+2), in tabs, with a sidebar of places and disks.
- Thumbnails for pictures; icons from the desktop's icon theme by file type.
- Copy, cut and paste (on the clipboard in Nautilus's form, so the two paste each other's), drag
  to move (Ctrl to copy; another disk copies), rename in place (F2), new folder (Ctrl+Shift+N).
- The Trash (freedesktop spec): Delete moves there; put back, delete permanently, empty.
- Click, Ctrl/Shift-click, a band on the background, arrows and typing a name to select.
- Path bar to click or drop on, Ctrl+L to type a location, Filter for the folder.
- Watches the folders on screen and re-reads them when they change.
- The network, through GVfs as in Nautilus: servers found on the network, a server's shares,
  mounting one (with a password, saved in the keyring if you like) and unmounting from the
  sidebar; `smb://` URIs in the location bar or Go → Connect to Server….
- Patina skins (Skin menu, as in every Patina app; `~/.config/files/skins`), a status bar, and
  the Patina frame or the desktop's.

- The file chooser for other apps: in the Patina session, any app that asks the desktop portal
  for a file (rfd's default, Firefox, Chrome, GTK and Qt apps set to use portals) gets Files'
  chooser: open, several files, a folder, or save, with the app's file types.

## Install

```sh
make install      # ~/.local/bin/files, its .desktop file and icon
make default      # optional: open folders in Files instead of Nautilus
make install-portal   # be the file chooser in the Patina session (root; SUDO=doas)
make app          # or target/Files.app, to copy into ~/Apps
```

It depends on [patina-apps](https://github.com/Sector-F-Labs/patina-apps) and Patina from GitHub.

## Develop

```sh
make check        # tests and clippy
make run
FILES_SCREENSHOT=shot.png cargo run -- ~/somewhere   # saves the window and quits
```

Decisions are in [docs/adr](docs/adr/README.md).
