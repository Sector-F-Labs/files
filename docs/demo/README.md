# Screenshots on made-up data

The website's screenshots show nothing of anyone's machine. `build_home.py` makes a home folder of
invented files (generated pictures, empty or sparse documents, `Name.app` folders with drawn icons).
`shot.sh` runs the release build of Files in a headless sway and a mount namespace where that folder
is `/home/demo`, the mount table says it is the ZFS dataset `tank/home`, and `bin/zfs` and
`bin/zpool` answer with invented numbers and snapshots. `FILES_SCREENSHOT` makes Files save its window.

    make screenshots     # builds, then rewrites docs/screenshots/*.png

Needs sway, `unshare` (user namespaces) and Python with Pillow. Add a screenshot by adding a line to
`screenshots.sh`.
