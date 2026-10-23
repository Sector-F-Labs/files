# 0003. One operations thread, and a name that's taken is never overwritten

Date: 2026-10-16

## Status

Accepted; its rule for taken names superseded by [ADR-0009](0009-taken-names-are-asked-about.md)

## Context

Copying and moving can take minutes and must not stop the window. Nautilus asks what to do
when a name is taken (replace, skip, keep both), which needs a dialog in the middle of an
operation running elsewhere.

## Decision

- Copy, move, trash, put back and delete-permanently run one at a time, in order, on a single
  worker thread. The status bar shows progress (bytes for copies), how many are waiting, and a
  Stop button that stops after the current file and removes a half-written one.
- A name that's taken is never overwritten: the newcomer gets the first free "name copy",
  "name copy 2", … with the extension kept at the end. New folders are "untitled folder",
  "untitled folder 2".
- A drag moves within a disk and copies to another; Ctrl copies. Moving across disks copies
  and then removes the original.
- The Trash follows the freedesktop spec through the trash crate.

## Consequences

- Nothing is lost by a paste or a drop, and there's no dialog in the middle of one; the cost is
  a "copy" to rename or delete by hand when replacing was meant.
- Two operations never race each other; a long copy holds up the next.
