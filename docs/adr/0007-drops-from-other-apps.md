# 0007. Files takes other apps' drags of files, so files can be dragged between Files windows

Date: 2026-10-17

## Status

Accepted. The user asked to drag from one Files window to another.

## Context

Each Files window is its own process (New Window runs `files` again), so to one window another is
just another app. ADR-0006 made Files' drags Wayland drags, but Files let go of every offer that
wasn't its own, so nothing could be dropped into it: not from another Files window, and not from
Nautilus or a browser. winit doesn't take Wayland drops either.

Files' drops into folders, places and the background are egui's: a payload of paths, highlighted
while it hovers over a target, and acted on when the button is released. ADR-0006 already passes
Files' own Wayland drag back to egui as pointer events while it's over the window.

## Decision

- Over Files' window, the second Wayland connection (`src/dnd.rs`, renamed from `drag_out.rs`)
  accepts any drag offering `text/uri-list`, with Copy and Move, preferring Move.
- The list is read **as soon as the drag comes over**, not at the drop. The local files in it
  become egui's payload while the drag is over the window, so folders and places light up as they
  do for Files' own drags, and the drop is egui's, through the same `Action::Drop` as inside one
  window. The drop waits up to two seconds for the list if it hasn't arrived yet. URIs that aren't
  local files are left out.
- Move or copy follows Files' rule: move on the same disk, copy across disks, and copy when the
  drop is a copy. A drop's action comes from the compositor. Patina's compositor chooses Copy with
  Ctrl held and Move with Shift, as GNOME does. Files' own drags now offer Move as well as Copy.
  The window the files are dropped in does the move, through its operations thread, which never
  overwrites. The window they came from deletes nothing, and sees them go through its folder
  watcher.
- The in-window ghost isn't drawn for an incoming drag, since the compositor draws the source's
  icon.

## Consequences

- Files can be dragged between Files windows, and in from Nautilus, browsers' downloads and
  anything else that offers file URIs.
- Reading the list before the drop asks the source for its data early. Every source tested
  answers at once, but a source that only answers at the drop would make it wait up to two
  seconds, and then nothing would be dropped.
- A drag offering only other types (text, images as data) isn't taken. Saving dragged data as a
  new file would be a new decision.
- Ctrl changes the action only when the target window learns of the drag, so it has to be held
  as the drag comes over, not pressed during the drop.
