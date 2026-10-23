# 0009. A name that's taken is asked about, as Nautilus does: folders merge, a replaced item goes to the Trash

Date: 2026-10-24

## Status

Accepted

## Context

ADR-0003 settled every taken name by calling the newcomer "name copy", so nothing was ever
overwritten and no dialog came up in the middle of an operation. That can't do what people do
most often with a folder that's already there: drag a newer `Photos` over the old one to add
what's new. Nautilus asks: two folders can be merged (asking again about each file taken
inside), a file can be replaced, skipped, or renamed, and an answer can apply to the rest.

We considered keeping ADR-0003 with "Merge" as the only addition, but replacing is what a merge
needs for its files to be updated. Deleting the replaced file, as Nautilus does, would break
the "nothing is lost" property that ADR-0003 kept.

## Decision

- When a copied or moved item's name is taken where it's going, the worker thread puts the
  conflict in its status and waits; the window shows a dialog and answers. Stop, or Cancel in
  the dialog, ends the operation.
- Two folders get **Merge**: what's in the newcomer goes into the one there, each taken name
  asked about in turn. A move then removes the folder it emptied, keeping what was skipped.
  An app (`.app`, ADR-0002) is one item and is never merged.
- Anything else gets **Replace**: the item there goes to the Trash first, never deleted. If it
  can't be trashed, the newcomer is left where it was and the error is reported.
- Both get **Keep Both** (the "name copy" of ADR-0003) and **Skip**, and "Do this for all
  folders/files", which holds for the rest of that operation.
- A copy into its own folder still makes "name copy" without asking.

This supersedes ADR-0003's rule that a taken name always becomes "name copy". One worker
thread and Stop stay as ADR-0003 decided.

## Consequences

- Updating a folder from a newer copy is one drag, as in Nautilus, and anything replaced can
  still be put back from the Trash.
- An operation can wait on the user while the queue behind it waits too. Escape cancels the
  operation rather than skipping one item.
- Replacing files on a disk without a usable Trash fails instead of overwriting. Merging very
  large trees asks at every taken name unless "for all" is ticked.
