# 0010. The column view: the tab's folder is the column with the keys, the others are read beside it

Date: 2026-10-24

## Status

Accepted. The user asked for a column view like the Mac's, and for this record of it.

## Context

The Finder's column view shows a folder's path as columns, one per folder, each with the folder
next on the way down marked; selecting a folder opens its items in a column to the right, and
selecting a file shows a preview there. Files had icons and a list, both drawing one tab's one
listing: `Tab` holds a location, its entries and a selection, and everything else (the keys, the
selection band, rename, Copy and Trash, Properties, Quick Look, the chooser's accept) acts on
those.

A column view shows several folders at once, so something has to say which one is "the" folder.
One way was to make a tab hold a list of columns, each with its own entries and selection, which
would mean teaching every action which column it's for. The other, taken, keeps the tab as it is
and makes it the column being worked in, as the Finder's window title follows that column.

Where the columns start also needed choosing. The Finder starts them where the window was pointed
(a sidebar place), which is history Files' tabs don't keep. The path bar already decides where a
path starts (Home inside it, the computer outside it, the network in a share), so the columns do
the same.

## Decision

- **The tab's folder is the column with the keys.** Its entries and selection are the tab's, so
  the arrow keys, the band, rename, the menus, Quick Look and the chooser work in it unchanged.
  Clicking an item or the space in another column (`Action::Column`) goes to that column's folder,
  through `go`, so Back and Forward step through columns, with that item selected. ← goes up and
  → goes into the selected folder at its first item.
- **The columns are the path bar's**: one for each folder from where `crumbs` starts the path down
  to the tab's folder, then the one selected folder's items, or the one selected file's preview
  (its icon or thumbnail, name, kind, size and date).
- **The other folders are read like any other** (`listing::spawn`, the same `Listed` channel) into
  `Files::column_lists`, kept only while they're shown and watched for changes while they are. The
  tab's own folder is kept there too, so when it becomes a column to the left it's already read,
  and going into a column's folder takes its listing at once instead of showing it empty until
  it's read again.
- **The Trash and the network stay a list**: they have no folders to go down through.
- Columns are a fixed width, in a horizontal scroll area kept at its right end; a column that's
  new is scrolled to show the way down in its middle.

## Consequences

- No action had to learn about columns beyond the clicks and ← and →; a later view built on one
  folder at a time can do the same.
- Every column change is a history step, as each was a `go`; Back can take many steps to leave a
  run of clicks down a path.
- Folders to the left are read again whenever they come into view anew; on a slow network share
  a column can show empty for a moment.
- → before the selected folder has been read goes into it with nothing selected.
- Columns can't be resized, and starting where the path bar does means a deep path inside Home
  always shows every folder from Home, scrolled.
