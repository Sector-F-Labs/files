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

/// What the side panel shows about a dataset: its pool, its snapshots and
/// more of its properties. Read only while the panel is open.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Details {
    pub pool: Option<Pool>,
    /// Oldest first.
    pub snapshots: Vec<Snapshot>,
    /// Label and value, in the order to show them.
    pub properties: Vec<(String, String)>,
    pub by_snapshots: u64,
    pub by_dataset: u64,
    pub by_children: u64,
    pub logical: u64,
    /// The last scrub or resilver, as `zpool status` words it.
    pub scan: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pool {
    pub name: String,
    pub health: String,
    pub size: u64,
    pub allocated: u64,
    pub free: u64,
    /// Percent, if the pool reports it.
    pub fragmentation: Option<u64>,
    pub dedup: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// After the `@`.
    pub name: String,
    pub used: u64,
    pub referenced: u64,
    /// Seconds since the epoch.
    pub created: i64,
}

pub struct DetailReader {
    send: Sender<(String, Details)>,
    done: Receiver<(String, Details)>,
}

impl DetailReader {
    pub fn new() -> Self {
        let (send, done) = channel();
        DetailReader { send, done }
    }

    pub fn start(&self, dataset: String, ctx: &egui::Context) {
        let (send, ctx) = (self.send.clone(), ctx.clone());
        std::thread::spawn(move || {
            let details = read_details(&dataset);
            let _ = send.send((dataset, details));
            ctx.request_repaint();
        });
    }

    pub fn poll(&self) -> Vec<(String, Details)> {
        self.done.try_iter().collect()
    }
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn read_details(dataset: &str) -> Details {
    let pool_name = dataset.split('/').next().unwrap_or(dataset);
    let props = run(
        "zfs",
        &[
            "get",
            "-Hp",
            "-o",
            "property,value",
            "mountpoint,atime,sync,copies,encryption,dedup,readonly,primarycache,logicalused,\
             usedbysnapshots,usedbydataset,usedbychildren,creation",
            dataset,
        ],
    )
    .unwrap_or_default();
    let snapshots = run(
        "zfs",
        &["list", "-Hp", "-t", "snapshot", "-d", "1", "-o", "name,used,referenced,creation", "-s", "creation", dataset],
    )
    .map(|t| parse_snapshots(&t))
    .unwrap_or_default();
    let pool =
        run("zpool", &["list", "-Hp", "-o", "name,health,size,allocated,free,fragmentation,dedupratio", pool_name])
            .and_then(|t| parse_pool(&t));
    let scan = run("zpool", &["status", pool_name])
        .and_then(|t| t.lines().find_map(|l| l.trim().strip_prefix("scan:").map(|s| s.trim().to_string())));
    parse_details(&props, snapshots, pool, scan)
}

fn parse_details(props: &str, snapshots: Vec<Snapshot>, pool: Option<Pool>, scan: Option<String>) -> Details {
    let get = |name: &str| {
        props.lines().find_map(|l| l.split_once('\t').filter(|(p, _)| *p == name).map(|(_, v)| v.trim().to_string()))
    };
    let number = |name: &str| get(name).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    let mut properties = Vec::new();
    for (label, name) in [
        ("Mount point", "mountpoint"),
        ("Encryption", "encryption"),
        ("Deduplication", "dedup"),
        ("Copies", "copies"),
        ("Sync", "sync"),
        ("Access times", "atime"),
        ("Read only", "readonly"),
        ("Primary cache", "primarycache"),
    ] {
        if let Some(v) = get(name) {
            properties.push((label.to_string(), v));
        }
    }
    Details {
        pool,
        snapshots,
        properties,
        by_snapshots: number("usedbysnapshots"),
        by_dataset: number("usedbydataset"),
        by_children: number("usedbychildren"),
        logical: number("logicalused"),
        scan,
    }
}

fn parse_snapshots(text: &str) -> Vec<Snapshot> {
    text.lines()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let name = f.next()?.split_once('@')?.1.to_string();
            Some(Snapshot {
                name,
                used: f.next()?.parse().ok()?,
                referenced: f.next()?.parse().ok()?,
                created: f.next()?.parse().ok()?,
            })
        })
        .collect()
}

fn parse_pool(text: &str) -> Option<Pool> {
    let mut f = text.lines().next()?.split('\t');
    Some(Pool {
        name: f.next()?.to_string(),
        health: f.next()?.to_string(),
        size: f.next()?.parse().ok()?,
        allocated: f.next()?.parse().ok()?,
        free: f.next()?.parse().ok()?,
        fragmentation: f.next()?.parse().ok(),
        dedup: f.next()?.trim_end_matches('x').to_string(),
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

    #[test]
    fn snapshots_and_pool_are_read() {
        let snaps = parse_snapshots("tank@a\t10\t20\t1000\ntank@b\t0\t30\t2000\n");
        assert_eq!(snaps.len(), 2);
        assert_eq!((snaps[1].name.as_str(), snaps[1].created), ("b", 2000));
        let pool = parse_pool("tank\tONLINE\t100\t60\t40\t43\t1.13x\n").unwrap();
        assert_eq!((pool.fragmentation, pool.dedup.as_str()), (Some(43), "1.13"));
        assert_eq!(parse_pool("tank\tONLINE\t100\t60\t40\t-\t1.00x\n").unwrap().fragmentation, None);
        let d = parse_details("usedbysnapshots\t5\ndedup\toff\n", snaps, None, None);
        assert_eq!((d.by_snapshots, d.properties.len()), (5, 1));
    }
}
