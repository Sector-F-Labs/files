#!/bin/bash
# One screenshot of Files on made-up data, for the website. See README.md here.
#   shot.sh NAME FOLDER 'config.toml text'   ->  $OUT/NAME.png   (SIZE=1180x740 by default)
# Needs sway, unshare and python3 with Pillow. The files binary is target/release/files.
set -e
HERE=$(cd "$(dirname "$0")" && pwd)
WORK=${WORK:-${TMPDIR:-/tmp}/files-demo}       # the made-up home and sway's scratch space
OUT=${OUT:-$HERE/../screenshots}
name=$1; folder=$2; config=$3
mkdir -p "$OUT" "$WORK"
[ -d "$WORK/home" ] || python3 "$HERE/build_home.py" "$WORK/home"
# Copied outside /home, which the namespace below hides.
cp "$HERE/../../target/release/files" "$WORK/files-bin"
rm -rf "$WORK/bin"; cp -r "$HERE/bin" "$WORK/bin"; cp "$HERE/mountinfo" "$WORK/mountinfo"
mkdir -p "$WORK/home/.config/files"
printf '%s\n' "$config" > "$WORK/home/.config/files/config.toml"
rm -rf "$WORK/home/.local" "$WORK/home/.cache"
# Wayland socket paths are short-limited, so the runtime dir is not under $WORK.
RT=$(mktemp -d /tmp/fdemo.XXXXXX); chmod 700 "$RT"
printf 'output HEADLESS-1 resolution %s\ndefault_border none\n' "${SIZE:-1180x740}" > "$WORK/sway.conf"
XDG_RUNTIME_DIR=$RT WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
    sway -c "$WORK/sway.conf" >"$WORK/sway.log" 2>&1 &
sway=$!
trap 'kill $sway 2>/dev/null; rm -rf "$RT"' EXIT
for _ in $(seq 50); do ls "$RT"/wayland-? >/dev/null 2>&1 && break; sleep 0.2; done
sock=$(basename "$(ls "$RT"/wayland-? | head -1)")
# A mount namespace of its own: /home is the made-up home as /home/demo, and the mount table says
# /home/demo is the ZFS dataset tank/home; bin/ holds stand-ins for zfs and zpool.
unshare -rm sh -c "mount -t tmpfs tmpfs /home && mkdir /home/demo && mount --bind '$WORK/home' /home/demo \
  && mount --bind '$WORK/mountinfo' /proc/\$\$/mountinfo && cd /home/demo \
  && exec env HOME=/home/demo USER=demo XDG_CONFIG_HOME=/home/demo/.config XDG_DATA_HOME=/home/demo/.local/share \
     XDG_CACHE_HOME=/home/demo/.cache XDG_RUNTIME_DIR=$RT WAYLAND_DISPLAY=$sock PATH='$WORK/bin':\$PATH \
     FILES_SCREENSHOT='$WORK/$name.png' '$WORK/files-bin' $folder" 2>&1 | grep -v 'D-Bus\|libEGL\|^$' || true
mv "$WORK/$name.png" "$OUT/$name.png"
