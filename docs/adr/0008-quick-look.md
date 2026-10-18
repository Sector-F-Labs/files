# 0008. Quick Look: Space shows the selected item in a window of its own

Date: 2026-10-18

## Status

Accepted. The user asked for Space on a selected picture to preview it, as on a Mac.

## Context

To see a picture, Files could only open it in another app (`xdg-open`), which is slow to flip
through and, until the session had its own defaults (patina-desktop ADR-0066), was the browser.
The Finder's Quick Look shows the selected item at once on Space and follows the arrow keys,
then Space or Escape puts it away.

On a Mac Quick Look is a window of its own, beside the folder, to move and resize. The first
version here was a panel drawn over the folder inside Files' window, which needed nothing new but
hid the folder; the user wanted the window that pops out. egui does this with a second viewport,
which on Wayland is another toplevel the desktop places. An immediate viewport runs inside Files'
own frame, so it reads and changes the same state, with nothing to pass between windows.

## Decision

- **Space toggles Quick Look** for the item at the cursor (or the first selected), unless a name is
  being typed, when the space goes on with it. Escape also closes it, and Enter opens the item as
  before. File → Quick Look does the same.
- **It's a window of its own** (an immediate viewport, `Files::quick_look`) with Patina's frame
  and skin: the item's name as its title, Open beside the window buttons, and its details along
  the bottom. Its app id is `com.sectorflabs.Files.QuickLook`, so the Patina desktop keeps its
  place apart from the folder windows'.
- **The folder's keys work in it** (`Files::keys` runs on its input too): the arrow keys move the
  selection and the window follows, Delete, copy and the rest act on the item. It closes with
  Space, Escape or its close button, when nothing is selected, or when a rename starts.
- **Pictures** (the types that get thumbnails) are decoded on a thread of their own
  (`src/preview.rs`), at the screen's size up to 4096 pixels, turned as their EXIF says, and
  shown at their own size or smaller to fit. Only the newest request is decoded, so holding an
  arrow key doesn't queue every picture passed, and the last eight are kept. SVGs have no size of
  their own and are drawn for the window, at most 1024 pixels, since resvg is slow.
- **Anything else** shows its icon, kind, size and date.

## Consequences

- Flipping through a folder of photos needs no other app. Thumbnails are turned by EXIF too, as
  they now share the decoder.
- The folder stays in view and can be clicked; the preview follows the selection either way.
- The desktop places the window, not Files: Wayland gives no way to put it beside the folder, and
  no app has a desktop entry under its app id, so the bar shows it without an icon.
- No preview yet for text, PDFs, video or audio: they get the icon card. Each would be its own
  renderer in the same window.
- An immediate viewport repaints with the folder window; fine for one picture, but anything that
  animates (video) would want a deferred one.
