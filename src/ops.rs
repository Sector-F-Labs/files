//! Copying, moving, trashing and restoring, one at a time on a thread of
//! their own, with progress the status bar shows and a way to stop. A name
//! that's taken is never overwritten: the newcomer is called "name copy".

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub enum Op {
    Copy {
        sources: Vec<PathBuf>,
        into: PathBuf,
    },
    Move {
        sources: Vec<PathBuf>,
        into: PathBuf,
    },
    Trash(Vec<PathBuf>),
    Restore(Vec<trash::TrashItem>),
    /// Deletes trashed items for good.
    Purge(Vec<trash::TrashItem>),
}

impl Op {
    fn label(&self) -> String {
        let count = |n: usize| if n == 1 { "1 item".to_string() } else { format!("{n} items") };
        match self {
            Op::Copy { sources, .. } => format!("Copying {}", count(sources.len())),
            Op::Move { sources, .. } => format!("Moving {}", count(sources.len())),
            Op::Trash(paths) => format!("Moving {} to the Trash", count(paths.len())),
            Op::Restore(items) => format!("Restoring {}", count(items.len())),
            Op::Purge(items) => format!("Deleting {}", count(items.len())),
        }
    }
}

/// What the status bar shows.
#[derive(Debug, Clone, Default)]
pub struct Status {
    /// What's running, with how far along it is (0 to 1) when that's known.
    pub running: Option<(String, Option<f32>)>,
    /// Operations waiting their turn.
    pub queued: usize,
    /// Errors from the last operation, until the next one starts.
    pub errors: Vec<String>,
    /// Where the last operation put things, for selecting them.
    pub placed: Vec<PathBuf>,
}

pub struct Worker {
    send: Sender<Op>,
    pub status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
}

impl Worker {
    pub fn new(ctx: &egui::Context) -> Self {
        let (send, receive) = channel::<Op>();
        let status = Arc::new(Mutex::new(Status::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (shared, stop, ctx) = (status.clone(), cancel.clone(), ctx.clone());
        std::thread::Builder::new()
            .name("operations".into())
            .spawn(move || {
                for op in receive {
                    stop.store(false, Ordering::Relaxed);
                    {
                        let mut s = lock(&shared);
                        s.queued = s.queued.saturating_sub(1);
                        s.running = Some((op.label(), None));
                        s.errors.clear();
                    }
                    ctx.request_repaint();
                    let mut run = Run { status: &shared, stop: &stop, ctx: &ctx, done: 0, total: 0 };
                    let (placed, errors) = run.op(op);
                    let mut s = lock(&shared);
                    s.running = None;
                    s.errors = errors;
                    s.placed = placed;
                    drop(s);
                    ctx.request_repaint();
                }
            })
            .ok();
        Worker { send, status, cancel }
    }

    pub fn start(&self, op: Op) {
        lock(&self.status).queued += 1;
        let _ = self.send.send(op);
    }

    /// Stops what's running after the file it's on.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

fn lock(status: &Mutex<Status>) -> std::sync::MutexGuard<'_, Status> {
    status.lock().unwrap_or_else(|e| e.into_inner())
}

struct Run<'a> {
    status: &'a Mutex<Status>,
    stop: &'a AtomicBool,
    ctx: &'a egui::Context,
    done: u64,
    total: u64,
}

impl Run<'_> {
    /// Runs `op`: what it placed, and what went wrong.
    fn op(&mut self, op: Op) -> (Vec<PathBuf>, Vec<String>) {
        let mut placed = Vec::new();
        let mut errors = Vec::new();
        match op {
            Op::Copy { sources, into } => {
                self.total = sources.iter().map(|s| size(s)).sum();
                for source in sources {
                    match self.copy_into(&source, &into) {
                        Ok(to) => placed.push(to),
                        Err(e) => errors.push(format!("{}: {e}", name(&source))),
                    }
                    if self.stopped() {
                        break;
                    }
                }
            }
            Op::Move { sources, into } => {
                for source in sources {
                    match self.move_into(&source, &into) {
                        Ok(to) => placed.push(to),
                        Err(e) => errors.push(format!("{}: {e}", name(&source))),
                    }
                    if self.stopped() {
                        break;
                    }
                }
            }
            Op::Trash(paths) => {
                for path in paths {
                    if let Err(e) = trash::delete(&path) {
                        errors.push(format!("{}: {e}", name(&path)));
                    }
                }
            }
            Op::Restore(items) => {
                for item in items {
                    let to = item.original_parent.join(&item.name);
                    match trash::os_limited::restore_all([item]) {
                        Ok(()) => placed.push(to),
                        Err(e) => errors.push(restore_error(e)),
                    }
                }
            }
            Op::Purge(items) => {
                if let Err(e) = trash::os_limited::purge_all(items) {
                    errors.push(e.to_string());
                }
            }
        }
        if self.stopped() {
            errors.push("Stopped".into());
        }
        (placed, errors)
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn copy_into(&mut self, source: &Path, into: &Path) -> std::io::Result<PathBuf> {
        refuse_into_itself(source, into)?;
        let to = free_name(into, &name(source));
        self.copy(source, &to)?;
        Ok(to)
    }

    /// Renames where it can; across disks, copies and then removes.
    fn move_into(&mut self, source: &Path, into: &Path) -> std::io::Result<PathBuf> {
        refuse_into_itself(source, into)?;
        if source.parent() == Some(into) {
            return Ok(source.to_path_buf());
        }
        let to = free_name(into, &name(source));
        match std::fs::rename(source, &to) {
            Ok(()) => Ok(to),
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                self.total = size(source);
                self.done = 0;
                self.copy(source, &to)?;
                if self.stopped() {
                    return Ok(to);
                }
                if std::fs::symlink_metadata(source)?.is_dir() {
                    std::fs::remove_dir_all(source)?;
                } else {
                    std::fs::remove_file(source)?;
                }
                Ok(to)
            }
            Err(e) => Err(e),
        }
    }

    /// Copies a file, a link or a folder and all in it to `to`.
    fn copy(&mut self, from: &Path, to: &Path) -> std::io::Result<()> {
        if self.stopped() {
            return Ok(());
        }
        let meta = std::fs::symlink_metadata(from)?;
        if meta.file_type().is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(from)?, to)
        } else if meta.is_dir() {
            std::fs::create_dir(to)?;
            for entry in std::fs::read_dir(from)? {
                let entry = entry?;
                self.copy(&entry.path(), &to.join(entry.file_name()))?;
            }
            std::fs::set_permissions(to, meta.permissions())
        } else {
            self.copy_file(from, to, meta.permissions())
        }
    }

    fn copy_file(&mut self, from: &Path, to: &Path, permissions: std::fs::Permissions) -> std::io::Result<()> {
        let mut reader = std::fs::File::open(from)?;
        let mut writer = std::fs::OpenOptions::new().write(true).create_new(true).open(to)?;
        let mut buffer = vec![0; 1 << 20];
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            writer.write_all(&buffer[..n])?;
            self.done += n as u64;
            self.report();
            if self.stopped() {
                drop(writer);
                let _ = std::fs::remove_file(to);
                return Ok(());
            }
        }
        writer.set_permissions(permissions)
    }

    fn report(&self) {
        if self.total == 0 {
            return;
        }
        let mut s = lock(self.status);
        if let Some((_, progress)) = &mut s.running {
            *progress = Some((self.done as f32 / self.total as f32).min(1.0));
        }
        drop(s);
        self.ctx.request_repaint();
    }
}

fn restore_error(e: trash::Error) -> String {
    match e {
        trash::Error::RestoreCollision { path, .. } => {
            format!("{} is already there", path.display())
        }
        e => e.to_string(),
    }
}

fn refuse_into_itself(source: &Path, into: &Path) -> std::io::Result<()> {
    if into.starts_with(source) {
        return Err(std::io::Error::other("can't go inside itself"));
    }
    Ok(())
}

fn name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Bytes in `path`, folders and all; links count as nothing.
fn size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if meta.is_dir() {
        std::fs::read_dir(path).map(|d| d.filter_map(|e| e.ok()).map(|e| size(&e.path())).sum()).unwrap_or(0)
    } else if meta.is_file() {
        meta.len()
    } else {
        0
    }
}

/// `name` in `dir`, or the first of "name copy", "name copy 2", … that
/// isn't taken. The extension stays at the end, and an app's `.app` too.
pub fn free_name(dir: &Path, name: &str) -> PathBuf {
    let taken = |n: &str| std::fs::symlink_metadata(dir.join(n)).is_ok();
    if !taken(name) {
        return dir.join(name);
    }
    let (stem, ext) = split_extension(name);
    let base = stem.strip_suffix(" copy").unwrap_or(stem);
    (1..)
        .map(|i| if i == 1 { format!("{base} copy{ext}") } else { format!("{base} copy {i}{ext}") })
        .find(|n| !taken(n))
        .map(|n| dir.join(n))
        .unwrap_or_else(|| dir.join(name))
}

/// "photo.jpg" as ("photo", ".jpg"); a leading dot isn't an extension.
pub fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => name.split_at(i),
        _ => (name, ""),
    }
}

/// Makes "untitled folder", or "untitled folder 2" and so on, in `dir`.
pub fn new_folder(dir: &Path) -> std::io::Result<PathBuf> {
    let path = (1..)
        .map(|i| if i == 1 { "untitled folder".to_string() } else { format!("untitled folder {i}") })
        .map(|n| dir.join(n))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .unwrap_or_else(|| dir.join("untitled folder"));
    std::fs::create_dir(&path)?;
    Ok(path)
}

/// Renames `from` to `name` beside it, refusing a name that's taken.
pub fn rename(from: &Path, name: &str) -> Result<PathBuf, String> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(format!("“{name}” can't be a name"));
    }
    let to = from.with_file_name(name);
    if to == from {
        return Ok(to);
    }
    if std::fs::symlink_metadata(&to).is_ok() {
        return Err(format!("“{name}” is already taken"));
    }
    std::fs::rename(from, &to).map_err(|e| e.to_string())?;
    Ok(to)
}

/// Whether two paths are on the same file system, so a drag between them
/// moves rather than copies.
pub fn same_disk(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);

    impl Dir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("files-ops-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run(op: Op) -> (Vec<PathBuf>, Vec<String>) {
        let (status, stop, ctx) = (Mutex::new(Status::default()), AtomicBool::new(false), egui::Context::default());
        Run { status: &status, stop: &stop, ctx: &ctx, done: 0, total: 0 }.op(op)
    }

    #[test]
    fn names_never_overwrite() {
        let dir = Dir::new("names");
        let d = &dir.0;
        assert_eq!(free_name(d, "a.txt"), d.join("a.txt"));
        std::fs::write(d.join("a.txt"), "").unwrap();
        assert_eq!(free_name(d, "a.txt"), d.join("a copy.txt"));
        std::fs::write(d.join("a copy.txt"), "").unwrap();
        assert_eq!(free_name(d, "a.txt"), d.join("a copy 2.txt"));
        assert_eq!(free_name(d, "a copy.txt"), d.join("a copy 2.txt"));
        std::fs::create_dir(d.join("Tool.app")).unwrap();
        assert_eq!(free_name(d, "Tool.app"), d.join("Tool copy.app"));
        std::fs::write(d.join(".rc"), "").unwrap();
        assert_eq!(free_name(d, ".rc"), d.join(".rc copy"));
    }

    #[test]
    fn copies_folders_whole_and_moves_them() {
        use std::os::unix::fs::PermissionsExt;
        let dir = Dir::new("copy");
        let d = &dir.0;
        std::fs::create_dir_all(d.join("src/App.app/bin")).unwrap();
        std::fs::write(d.join("src/App.app/bin/run"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(d.join("src/App.app/bin/run"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("bin/run", d.join("src/App.app/link")).unwrap();
        std::fs::create_dir(d.join("dest")).unwrap();

        let (placed, errors) = run(Op::Copy { sources: vec![d.join("src/App.app")], into: d.join("dest") });
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(placed, [d.join("dest/App.app")]);
        let run_mode = std::fs::metadata(d.join("dest/App.app/bin/run")).unwrap().permissions().mode();
        assert_eq!(run_mode & 0o777, 0o755);
        assert_eq!(std::fs::read_link(d.join("dest/App.app/link")).unwrap(), Path::new("bin/run"));

        // Again: the copy gets a free name.
        let (placed, _) = run(Op::Copy { sources: vec![d.join("src/App.app")], into: d.join("dest") });
        assert_eq!(placed, [d.join("dest/App copy.app")]);

        let (placed, errors) = run(Op::Move { sources: vec![d.join("src/App.app")], into: d.join("dest") });
        assert!(errors.is_empty());
        assert_eq!(placed, [d.join("dest/App copy 2.app")]);
        assert!(!d.join("src/App.app").exists());

        let (_, errors) = run(Op::Move { sources: vec![d.join("dest")], into: d.join("dest/App.app") });
        assert!(errors[0].contains("inside itself"));
    }

    #[test]
    fn renames_only_to_free_good_names() {
        let dir = Dir::new("rename");
        let d = &dir.0;
        std::fs::write(d.join("a"), "").unwrap();
        std::fs::write(d.join("b"), "").unwrap();
        assert!(rename(&d.join("a"), "b").unwrap_err().contains("taken"));
        assert!(rename(&d.join("a"), "x/y").is_err());
        assert!(rename(&d.join("a"), "").is_err());
        assert_eq!(rename(&d.join("a"), "c").unwrap(), d.join("c"));
        assert_eq!(new_folder(d).unwrap(), d.join("untitled folder"));
        assert_eq!(new_folder(d).unwrap(), d.join("untitled folder 2"));
    }
}
