# 0006. Dragging files is a Wayland drag, started on a connection of Files' own

Date: 2026-10-16

## Status

Accepted. The user asked to be able to drag pictures from Files into Telegram, as they can from
Nautilus.

## Context

Dragging in Files was egui's own drag and drop (`DragAndDrop::set_payload`). It only reaches
widgets inside the window, so nothing outside it ever heard of a drag, and dropping onto Telegram
did nothing. Files also painted its own "ghost" label beside the pointer.

On Wayland, a drag to another app is a `wl_data_device.start_drag` from the client. It needs the
serial of the pointer press it continues, and a data source that writes the files (as
`text/uri-list`) when the target asks. winit, which eframe runs on, can't start a Wayland drag
and doesn't expose the serials. smithay-clipboard, which egui uses for the clipboard on Wayland,
has the same problem and solves it by opening its own connection onto winit's `wl_display`
(`Backend::from_foreign_display`), with its own seat and event queue.

The first version started the Wayland drag only once the pointer left the window, and kept
egui's drag inside it. That missed windows lying over Files, where the pointer never leaves Files'
rectangle, and it isn't how GNOME behaves: a drag there is the compositor's from the start.

## Decision

- `src/drag_out.rs` opens a second connection onto winit's display. On its own queue it binds the
  seat, a pointer (only for the serial of the press), the data-device manager, wl_compositor and
  wl_shm, and dispatches that queue on a thread. The thread answers the target's `send` (on a
  thread of its own, writing the URI list) however idle the window is.
- When egui starts dragging files, Files starts a Wayland drag straight away from its window
  surface. The drag offers `text/uri-list` (CR LF lines, RFC 2483), with Copy as the only action,
  so a drop elsewhere never deletes anything. The icon under the pointer is the item's own icon,
  taken from the pixels Files already decoded for the grid (`Icons::drawn`), or drawn for the drag
  if there are none.
- While the drag is over Files' own window, Files accepts its own offer, and the drag's enter,
  motion, leave and drop events come back to the app (`Over`). They're given to egui as pointer
  moves and a release, so egui's own drops (into folders, onto places, the copy modifier) work as
  they did. A drag that ends elsewhere, or is called off, releases egui's drag well away from
  anything.
- The in-window ghost is drawn only when there's no Wayland drag (X11), since the compositor
  draws the icon.

## Consequences

- Files can be dragged into any Wayland app that takes files: Telegram, browsers, Nautilus, text
  editors. This works on GNOME as well as the Patina desktop, where the compositor now draws drag
  icons (patina-desktop's `dnd.rs`).
- Files talks Wayland directly beside winit. That's two connections' worth of objects on one
  socket, and protocol code to keep up with.
- Drags from other apps into Files aren't taken yet: offers from other clients are let go. Taking
  them would use this same connection.
- The drag offers Copy only. Moving files out to another app by dragging would need Files to
  delete them after the target's `finish`. That's left out until there's a reason for it.
- On X11, drags still stay inside the window.
