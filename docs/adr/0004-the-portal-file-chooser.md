# 0004. Files is the Patina session's file chooser, through the desktop portal

Date: 2026-10-16

## Status

Accepted

## Context

Messenger's "Send an image" opened GTK's file chooser even in the Patina session. Like most apps
now (rfd by default, Firefox, Chrome, Flatpaks), it asks xdg-desktop-portal for a file, and the
portal hands the request to a backend chosen per desktop by `<desktop>-portals.conf`. Patina had
none, so GTK's answered.

The other option was a chooser in Patina, the library, that Patina apps call instead of rfd. It
would only reach our own apps, and each would carry a second file browser.

## Decision

- Files implements the portal's backend interface, `org.freedesktop.impl.portal.FileChooser`
  (OpenFile, SaveFile, SaveFiles), as `files --portal`, a D-Bus-activated service named
  `org.freedesktop.impl.portal.desktop.patina`.
- Each request starts `files --choose`: Files' own window in a picking mode (title from the app,
  no menus or tabs, the app's file types, a name field for saving, Cancel and the app's accept
  label), given the request as TOML on stdin and printing the answer on stdout. A process per
  dialog because winit allows one event loop per process; the portal's Close kills it.
- `patina-portals.conf` picks Patina for FileChooser and GTK for everything else, so it applies
  only where `XDG_CURRENT_DESKTOP=Patina`, which the compositor exports to D-Bus. GNOME keeps
  its own chooser.
- The chooser starts where the app asks, else where the last choice was made.

## Consequences

- Every portal-using app in the Patina session gets the same chooser as Files, skins included.
- The `.portal` file and the config must be in `/usr/share/xdg-desktop-portal`, so installing
  takes root once (`make install-portal`); in Patina OS they belong in the image.
- Apps that bypass the portal (GTK3 apps without `GTK_USE_PORTAL`, some Qt apps) still show
  their toolkit's own dialog.
- The service exits after five idle minutes; the first dialog after that waits for it to start.
