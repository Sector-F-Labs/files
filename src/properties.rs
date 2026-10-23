//! The Properties window's facts, as Nautilus shows them: each item's
//! details from `stat`, its owner and group by name, the disk's free space,
//! and what's inside its folders counted up. All of it is read on a thread,
//! since a folder can hold a disk's worth; the window shows the count so far,
//! and counting stops when the window closes. A change of permissions is
//! made on a thread too, and the item read again.

use std::ffi::{CStr, CString};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use crate::listing::{self, Entry, Kind};
use crate::mime::Mime;

/// How often the count so far is shown.
const SHOW_EVERY: Duration = Duration::from_millis(100);

/// One item's details.
#[derive(Clone)]
pub struct Item {
    pub entry: Entry,
    /// Its permission bits (`0o7777`).
    pub mode: u32,
    pub uid: u32,
    pub owner: String,
    pub group: String,
    pub accessed: Option<SystemTime>,
    pub created: Option<SystemTime>,
    /// Where a link points.
    pub target: Option<PathBuf>,
    /// For a folder: the free and total bytes of the disk it's on.
    pub disk: Option<(u64, u64)>,
}

impl Item {
    pub fn is_folder(&self) -> bool {
        self.entry.kind != Kind::File
    }
}

/// What's known so far.
#[derive(Clone, Default)]
pub struct Info {
    /// Whether the items have been read.
    pub read: bool,
    pub items: Vec<Item>,
    /// Items that couldn't be read, and a change of permissions that failed.
    pub errors: Vec<String>,
    /// Inside the folders: files and folders, and the files' bytes (with
    /// the files chosen themselves).
    pub files: u64,
    pub folders: u64,
    pub bytes: u64,
    /// Folders inside that couldn't be read.
    pub unreadable: u64,
    pub counting: bool,
}

impl Info {
    /// "4.2 MB (4,213,077 bytes)" for a file, "12 items, totalling 4.2 MB"
    /// for a folder's contents or for several items.
    pub fn size(&self) -> String {
        if let [item] = &self.items[..]
            && !item.is_folder()
        {
            return with_bytes(item.entry.size);
        }
        let count = match &self.items[..] {
            [_] => self.files + self.folders,
            items => items.len() as u64,
        };
        let items = if count == 1 { "1 item".to_string() } else { format!("{} items", grouped(count)) };
        let more = if self.counting { "…" } else { "" };
        format!("{items}, totalling {}{more}", listing::human_size(self.bytes))
    }
}

/// The Properties of some items, being read.
pub struct Properties {
    info: Arc<Mutex<Info>>,
    stop: Arc<AtomicBool>,
    mime: Arc<Mime>,
    ctx: egui::Context,
}

impl Properties {
    pub fn new(paths: Vec<PathBuf>, mime: Arc<Mime>, ctx: &egui::Context) -> Self {
        let info = Arc::new(Mutex::new(Info { counting: true, ..Info::default() }));
        let stop = Arc::new(AtomicBool::new(false));
        let properties = Properties { info: info.clone(), stop: stop.clone(), mime: mime.clone(), ctx: ctx.clone() };
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("properties".into())
            .spawn(move || {
                let mut items = Vec::new();
                let mut errors = Vec::new();
                for path in &paths {
                    match item(path, &mime) {
                        Ok(item) => items.push(item),
                        Err(e) => errors.push(format!("Can't read “{}”: {e}", path.display())),
                    }
                }
                {
                    let mut info = lock(&info);
                    info.read = true;
                    info.items = items;
                    info.errors = errors;
                }
                ctx.request_repaint();
                count(&paths, &info, &stop, &ctx);
                lock(&info).counting = false;
                ctx.request_repaint();
            })
            .ok();
        properties
    }

    pub fn info(&self) -> MutexGuard<'_, Info> {
        lock(&self.info)
    }

    /// Gives the one item `mode`, then reads it again.
    pub fn set_mode(&self, mode: u32) {
        let Some(path) = self.info().items.first().map(|i| i.entry.path.clone()) else { return };
        let (info, mime, ctx) = (self.info.clone(), self.mime.clone(), self.ctx.clone());
        std::thread::Builder::new()
            .name("chmod".into())
            .spawn(move || {
                let done = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode));
                let again = item(&path, &mime);
                let mut info = lock(&info);
                if let Err(e) = done {
                    info.errors.push(format!("Can't change the permissions: {e}"));
                }
                if let (Ok(again), Some(first)) = (again, info.items.first_mut()) {
                    *first = again;
                }
                ctx.request_repaint();
            })
            .ok();
    }
}

impl Drop for Properties {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn lock(info: &Mutex<Info>) -> MutexGuard<'_, Info> {
    info.lock().unwrap_or_else(|e| e.into_inner())
}

/// An item as the listing shows it, with what only Properties shows.
fn item(path: &Path, mime: &Mime) -> std::io::Result<Item> {
    let link = std::fs::symlink_metadata(path)?;
    let symlink = link.is_symlink();
    // A link is shown as what it points to; a broken one as itself.
    let meta = std::fs::metadata(path).unwrap_or(link);
    let entry = listing::entry(path.to_path_buf(), &meta, symlink, mime);
    let disk = meta.is_dir().then(|| disk(path)).flatten();
    Ok(Item {
        entry,
        mode: meta.mode() & 0o7777,
        uid: meta.uid(),
        owner: user_name(meta.uid()).unwrap_or_else(|| meta.uid().to_string()),
        group: group_name(meta.gid()).unwrap_or_else(|| meta.gid().to_string()),
        accessed: meta.accessed().ok(),
        created: meta.created().ok(),
        target: symlink.then(|| std::fs::read_link(path).ok()).flatten(),
        disk,
    })
}

/// Counts everything inside `paths`, adding to `info` as it goes. Links
/// inside aren't followed.
fn count(paths: &[PathBuf], info: &Mutex<Info>, stop: &AtomicBool, ctx: &egui::Context) {
    let (mut files, mut folders, mut bytes, mut unreadable) = (0, 0, 0, 0);
    let mut shown = Instant::now();
    let mut todo = Vec::new();
    for path in paths {
        match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => todo.push(path.clone()),
            Ok(meta) => bytes += meta.len(),
            Err(_) => {}
        }
    }
    while let Some(dir) = todo.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            unreadable += 1;
            continue;
        };
        for item in read.flatten() {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            match item.metadata() {
                Ok(meta) if meta.is_dir() => {
                    folders += 1;
                    todo.push(item.path());
                }
                Ok(meta) => {
                    files += 1;
                    bytes += meta.len();
                }
                Err(_) => files += 1,
            }
            if shown.elapsed() > SHOW_EVERY {
                shown = Instant::now();
                let mut info = lock(info);
                (info.files, info.folders, info.bytes, info.unreadable) = (files, folders, bytes, unreadable);
                drop(info);
                ctx.request_repaint();
            }
        }
    }
    let mut info = lock(info);
    (info.files, info.folders, info.bytes, info.unreadable) = (files, folders, bytes, unreadable);
}

/// The free and total bytes of the disk `path` is on.
fn disk(path: &Path) -> Option<(u64, u64)> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } != 0 {
        return None;
    }
    let block = stat.f_frsize as u64;
    Some((stat.f_bavail as u64 * block, stat.f_blocks as u64 * block))
}

/// A user's name, from the system's user database.
fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(uid, &mut pwd, buf.as_mut_ptr(), buf.len(), &mut found) };
    if rc != 0 || found.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(pwd.pw_name) }.to_string_lossy().into_owned())
}

/// A group's name, from the system's group database.
fn group_name(gid: u32) -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    let rc = unsafe { libc::getgrgid_r(gid, &mut grp, buf.as_mut_ptr(), buf.len(), &mut found) };
    if rc != 0 || found.is_null() {
        return None;
    }
    Some(unsafe { CStr::from_ptr(grp.gr_name) }.to_string_lossy().into_owned())
}

/// Whether this user may change `item`'s permissions: its owner, or root.
pub fn may_change(item: &Item) -> bool {
    let me = unsafe { libc::geteuid() };
    me == 0 || me == item.uid
}

/// The access an owner, group or others can be given, as Nautilus names
/// it, by the `rwx` bits each stands for; and the bits they set.
pub fn levels(folder: bool) -> (&'static [(&'static str, u32)], u32) {
    if folder {
        (&[("None", 0), ("List files only", 4), ("Access files", 5), ("Create and delete files", 7)], 7)
    } else {
        // A file's `x` is the executable box's.
        (&[("None", 0), ("Read only", 4), ("Read and write", 6)], 6)
    }
}

/// The access `mode` gives the class at `shift` (6 the owner, 3 the group,
/// 0 others): its level's name, or its bits as `ls` shows them.
pub fn access(mode: u32, shift: u32, folder: bool) -> (u32, String) {
    let (levels, mask) = levels(folder);
    let bits = (mode >> shift) & mask;
    let name = levels.iter().find(|(_, b)| *b == bits).map_or_else(
        || {
            let all = (mode >> shift) & 7;
            [(4, 'r'), (2, 'w'), (1, 'x')].iter().map(|&(b, c)| if all & b != 0 { c } else { '-' }).collect()
        },
        |(name, _)| name.to_string(),
    );
    (bits, name)
}

/// `mode` with the class at `shift` given `bits`.
pub fn with_access(mode: u32, shift: u32, folder: bool, bits: u32) -> u32 {
    let (_, mask) = levels(folder);
    mode & !(mask << shift) | (bits & mask) << shift
}

/// `mode` made executable, by whoever can read it (the owner at least), or
/// by no one.
pub fn with_executable(mode: u32, on: bool) -> u32 {
    if on { mode | (mode & 0o444) >> 2 | 0o100 } else { mode & !0o111 }
}

/// "1,234,567".
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "4.2 MB (4,213,077 bytes)", or just "812 bytes".
fn with_bytes(bytes: u64) -> String {
    let human = listing::human_size(bytes);
    if bytes < 1000 { human } else { format!("{human} ({} bytes)", grouped(bytes)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_grouped_in_threes() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1000), "1,000");
        assert_eq!(grouped(4_213_077), "4,213,077");
        assert_eq!(with_bytes(812), "812 bytes");
        assert_eq!(with_bytes(4_213_077), "4.2 MB (4,213,077 bytes)");
    }

    #[test]
    fn access_reads_and_writes_one_class() {
        assert_eq!(access(0o644, 6, false), (6, "Read and write".into()));
        assert_eq!(access(0o644, 3, false), (4, "Read only".into()));
        assert_eq!(access(0o755, 0, true), (5, "Access files".into()));
        assert_eq!(access(0o620, 3, false).1, "-w-", "a mix with no name is shown as bits");
        assert_eq!(with_access(0o644, 3, false, 6), 0o664);
        assert_eq!(with_access(0o755, 0, false, 0), 0o751, "a file's x bit is left alone");
        assert_eq!(with_access(0o755, 0, true, 0), 0o750);
        assert_eq!(with_access(0o4755, 6, true, 5), 0o4555, "setuid stays");
    }

    #[test]
    fn executable_follows_who_can_read() {
        assert_eq!(with_executable(0o644, true), 0o755);
        assert_eq!(with_executable(0o600, true), 0o700);
        assert_eq!(with_executable(0o044, true), 0o155, "the owner always can");
        assert_eq!(with_executable(0o755, false), 0o644);
    }

    #[test]
    fn a_folder_is_counted() {
        let dir = std::env::temp_dir().join(format!("files-properties-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("one"), [0; 10]).unwrap();
        std::fs::write(dir.join("a/b/two"), [0; 5]).unwrap();
        let info = Mutex::new(Info::default());
        count(std::slice::from_ref(&dir), &info, &AtomicBool::new(false), &egui::Context::default());
        let info = info.into_inner().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((info.files, info.folders, info.bytes), (2, 2, 15));
    }
}
