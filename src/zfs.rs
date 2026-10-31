//! What the status bar says about a folder on ZFS: its dataset and the
//! dataset's space, compression and record size. The dataset is the source
//! of the mount the folder lies under (`/proc/self/mountinfo`); the numbers
//! come from `zfs get`. Both are read on a thread.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{Receiver, Sender, channel};

#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub dataset: String,
    pub used: u64,
    pub available: u64,
    /// What this dataset alone refers to, without its snapshots and children.
    pub referenced: u64,
    /// The dataset's size limit, if it has one.
    pub quota: Option<u64>,
    pub compression: String,
    /// "2.32", as `zfs` says it.
    pub ratio: String,
    pub recordsize: u64,
}

pub struct Reader {
    send: Sender<(PathBuf, Option<Info>)>,
    done: Receiver<(PathBuf, Option<Info>)>,
}

impl Reader {
    pub fn new() -> Self {
        let (send, done) = channel();
        Reader { send, done }
    }

    /// Reads about `folder` on a thread; `poll` has the answer.
    pub fn start(&self, folder: PathBuf, ctx: &egui::Context) {
        let (send, ctx) = (self.send.clone(), ctx.clone());
        std::thread::spawn(move || {
            let info = dataset_of(&folder).and_then(|dataset| read(&dataset));
            let _ = send.send((folder, info));
            ctx.request_repaint();
        });
    }

    pub fn poll(&self) -> Vec<(PathBuf, Option<Info>)> {
        self.done.try_iter().collect()
    }
}

/// The dataset mounted at the deepest mount point above `path`, if that
/// mount is ZFS.
pub fn dataset_of(path: &Path) -> Option<String> {
    let path = std::fs::canonicalize(path).ok()?;
    dataset_in(&std::fs::read_to_string("/proc/self/mountinfo").ok()?, &path)
}

fn dataset_in(mountinfo: &str, path: &Path) -> Option<String> {
    let mut best: Option<(usize, String, bool)> = None;
    for line in mountinfo.lines() {
        // "36 35 0:30 / /mnt rw,... shared:1 - zfs tank/x rw,..."
        let (before, after) = line.split_once(" - ")?;
        let mount = unescape(before.split(' ').nth(4)?);
        let mut after = after.split(' ');
        let (fstype, source) = (after.next()?, unescape(after.next()?));
        if !path.starts_with(&mount) {
            continue;
        }
        let depth = Path::new(&mount).components().count();
        // Later lines mount over earlier ones at the same place.
        if best.as_ref().is_none_or(|(d, ..)| depth >= *d) {
            best = Some((depth, source, fstype == "zfs"));
        }
    }
    best.filter(|(.., zfs)| *zfs).map(|(_, source, _)| source)
}

/// mountinfo writes a space, tab, newline or backslash as `\040` and so on.
fn unescape(s: &str) -> String {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && let Some(code) = s.get(i + 1..i + 4).and_then(|d| u8::from_str_radix(d, 8).ok())
        {
            out.push(code);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn read(dataset: &str) -> Option<Info> {
    let out = Command::new("zfs")
        .args([
            "get",
            "-Hp",
            "-o",
            "property,value",
            "used,available,referenced,quota,compression,compressratio,recordsize",
        ])
        .arg(dataset)
        .output()
        .ok()?;
    out.status.success().then(|| parse(dataset, &String::from_utf8_lossy(&out.stdout)))?
}

fn parse(dataset: &str, text: &str) -> Option<Info> {
    let get = |name: &str| {
        text.lines().find_map(|l| l.split_once('\t').filter(|(p, _)| *p == name).map(|(_, v)| v.trim().to_string()))
    };
    let number = |name: &str| get(name)?.parse::<u64>().ok();
    Some(Info {
        dataset: dataset.to_string(),
        used: number("used")?,
        available: number("available")?,
        referenced: number("referenced")?,
        quota: number("quota").filter(|q| *q > 0),
        compression: get("compression")?,
        ratio: get("compressratio")?.trim_end_matches('x').to_string(),
        recordsize: number("recordsize").unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTS: &str = "\
24 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
50 24 0:40 / /tank rw,xattr shared:2 - zfs tank rw,xattr
51 50 0:41 / /tank/games/gog/Skyrim\\040Special rw,xattr shared:3 - zfs tank/games/skyrim rw
52 24 0:42 / /home/divan/Src rw,xattr shared:4 - zfs tank/Src rw,xattr
";

    #[test]
    fn the_deepest_mount_gives_the_dataset() {
        let find = |p: &str| dataset_in(MOUNTS, Path::new(p));
        assert_eq!(find("/tank/photos/a"), Some("tank".into()));
        assert_eq!(find("/tank/games/gog/Skyrim Special/data"), Some("tank/games/skyrim".into()));
        assert_eq!(find("/home/divan/Src/files"), Some("tank/Src".into()));
        assert_eq!(find("/home/divan"), None, "ext4");
        assert_eq!(find("/tankful"), None, "not under /tank");
    }

    #[test]
    fn zfs_get_is_read() {
        let text = "used\t100\navailable\t900\nreferenced\t60\nquota\t0\ncompression\tlz4\ncompressratio\t2.32x\nrecordsize\t131072\n";
        let info = parse("tank/x", text).unwrap();
        assert_eq!((info.used, info.quota, info.ratio.as_str()), (100, None, "2.32"));
        assert!(parse("tank/x", "used\t1\n").is_none());
    }
}
