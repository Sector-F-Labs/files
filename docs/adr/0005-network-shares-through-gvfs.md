# 0005. Network shares through GVfs, as Nautilus has them, and opened as folders under its FUSE bridge

Date: 2026-10-16

## Status

Accepted. The user asked for Samba shares "just like nautilus would do", and accepted this
approach once it worked against their server.

## Context

The user asked for Files to find Samba shares on the network and mount them "just like nautilus
would do". Nautilus does it through GVfs: `network:///` lists the servers that DNS-SD and SMB
browsing find, `smb://host/` lists a server's shares, mounting one asks for a password through a
`GMountOperation` (which can save it in the keyring), and every mount also appears as a folder
under GVfs's FUSE bridge, `/run/user/<uid>/gvfs`. GVfs runs in any session that has GLib apps,
and on this machine it already held a share of urd's that Nautilus had mounted.

Ruled out:

- **`mount.cifs`**, the kernel's client: it needs root or an fstab line per share, and doesn't
  browse.
- **libsmbclient (or `smbclient`) directly**: Files would have its own mounts that Nautilus and
  other apps can't see, and every operation (listing, copying, the Trash) would need a second
  code path for files that aren't on a local path.
- **The `gio` command line**: it asks for passwords on stdin, with prompts we'd have to parse,
  and can't save them in the keyring.

## Decision

- Files uses GVfs through the `gio` crate, on a thread of its own running a GLib main loop
  (`src/network.rs`). The window sends it requests and gets listings (on the same channel folder
  listings use), mount results, questions and the current mounts back.
- The network and a server are a new kind of location, `Location::Network(uri)`, listing
  servers and shares as items. Opening a share mounts it (`mount_enclosing_volume`), then goes to
  its FUSE folder: from there on it's an ordinary folder, and copying, moving, the Trash,
  renaming, thumbnails and watching work on it unchanged. A share that's already mounted just
  opens.
- A server's password or question comes to the window as a dialog in Nautilus's form: registered
  user or anonymous, username, domain, password, and forget, remember until logout or remember
  forever. Cancelling is quiet.
- The sidebar gets a Network section: the Network place, then every mounted network share (from
  GIO's volume monitor, so mounts made by Nautilus or `gio mount` show too), each with an eject
  button. Tabs in a share that's unmounted go to its server. The path bar types `smb://` URIs, and
  Go → Connect to Server… starts one.

## Consequences

- Shares are shared with the rest of the desktop: what Nautilus mounted, Files shows, and the
  other way round, with the same saved passwords.
- Files now links GLib and GIO, and network browsing needs GVfs running, with `gvfs-backends` for
  SMB and `gvfs-fuse` for the folders. Without gvfsd-fuse a share mounts but can't be opened,
  which Files says. In Patina OS they belong in the image.
- Files over FUSE are slower than local ones, and inotify doesn't see changes made on the server
  by others, so a share's folder isn't re-read for them.
- Browsing depends on what GVfs finds: SMB servers that advertise neither DNS-SD nor WS-Discovery
  don't appear, and have to be typed (`smb://host/`).
- This is Linux (and FreeBSD with GVfs) only, which Files already is.
