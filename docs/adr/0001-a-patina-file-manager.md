# 0001. Write our own file manager on Patina, because Nautilus can't open .app folders

Date: 2026-10-16

## Status

Accepted

## Context

Patina OS's apps are folders, `~/Apps/Name.app` (its ADR-0002, ADR-0005). The Patina desktop's
launcher lists them, but in Nautilus a `.app` is just a folder: double-clicking opens it, and
there's no idea of a folder that acts as one file. The first idea was a thumbnailer so Nautilus
would at least show the app's icon. It would only give the icon, Nautilus runs thumbnailers in a
bubblewrap sandbox that sees only `/usr`, so the binary would need installing system-wide, and
with no MIME type for a folder it would be called for every folder.

A file manager of our own is a far bigger job (views, copying with progress, the Trash, drag and
drop, previews), but it's the only way to get the Mac's Finder behaviour, and Patina already has
the frame, tabs and skins.

## Decision

Files is a Patina app in its own private repository, installable per user on any Ubuntu (or
FreeBSD) desktop, not only Patina OS. It covers the everyday: browsing in tabs, icons and list,
thumbnails, copy/cut/paste, drag and drop, rename, new folder, the Trash, and `.app` folders as
apps. Remote locations (GVfs), "Open With" and file properties are left out for now.

## Consequences

- `.app` folders work wherever Files is installed, with nothing installed system-wide.
- We own a large app: everything Nautilus gets from GIO and GVfs we write or go without.
- Nautilus can stay as the default for folders, or Files can take over (`make default`).
