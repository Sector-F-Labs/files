//! Copying, moving, trashing and restoring, one at a time on a thread of
//! their own, with progress the status bar shows and a way to stop. A name
//! that's taken is asked about, as Nautilus does (ADR-0009): folders can be
//! merged, and a file replaced puts the one that was there in the Trash.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// An item going where its name is taken, which the window asks about.
#[derive(Debug, Clone)]
pub struct Conflict {
    pub source: PathBuf,
    pub existing: PathBuf,
    /// Both are folders (apps aren't), so they can be merged.
    pub folders: bool,
    /// Each one's size, a file's only, and when it was changed, for the
    /// window to show without reading the disk.
    pub sizes: [Option<u64>; 2],
    pub modified: [Option<std::time::SystemTime>; 2],
}

impl Conflict {
    fn new(source: &Path, existing: &Path) -> Self {
        let meta = [source, existing].map(|p| std::fs::symlink_metadata(p).ok());
        Conflict {
            source: source.to_path_buf(),
            existing: existing.to_path_buf(),
            folders: is_folder(source) && is_folder(existing),
            sizes: meta.clone().map(|m| m.filter(|m| m.is_file()).map(|m| m.len())),
            modified: meta.map(|m| m.and_then(|m| m.modified().ok())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    /// The folder's contents go into the one there, asking about each name
    /// taken in it.
    Merge,
    /// The one there goes to the Trash.
    Replace,
    /// The newcomer gets a free name, "name copy".
    KeepBoth,
    Skip,
    /// Stops the whole operation.
    Cancel,
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
    /// A name that's taken, waiting for `Worker::answer`.
    pub conflict: Option<Conflict>,
}

pub struct Worker {
    send: Sender<Op>,
    pub status: Arc<Mutex<Status>>,
    cancel: Arc<AtomicBool>,
    answer: Sender<(Choice, bool)>,
}

impl Worker {
    pub fn new(ctx: &egui::Context) -> Self {
        let (send, receive) = channel::<Op>();
        let status = Arc::new(Mutex::new(Status::default()));
        let cancel = Arc::new(AtomicBool::new(false));
        let (answer, answers) = channel();
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
                    let (placed, errors) = Run::new(&shared, &stop, &ctx, &answers).op(op);
                    let mut s = lock(&shared);
                    s.running = None;
                    s.errors = errors;
                    s.placed = placed;
                    drop(s);
                    ctx.request_repaint();
                }
            })
            .ok();
        Worker { send, status, cancel, answer }
    }

    pub fn start(&self, op: Op) {
        lock(&self.status).queued += 1;
        let _ = self.send.send(op);
    }

    /// Stops what's running after the file it's on.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Answers the conflict the status holds; with `all`, the same answer
    /// goes to the rest of its kind (folders, or files) in this operation.
    pub fn answer(&self, choice: Choice, all: bool) {
        lock(&self.status).conflict = None;
        let _ = self.answer.send((choice, all));
    }
}

fn lock(status: &Mutex<Status>) -> std::sync::MutexGuard<'_, Status> {
    status.lock().unwrap_or_else(|e| e.into_inner())
}

struct Run<'a> {
    status: &'a Mutex<Status>,
    stop: &'a AtomicBool,
    ctx: &'a egui::Context,
    answers: &'a Receiver<(Choice, bool)>,
    done: u64,
    total: u64,
    /// Answers given "for all": to folders, and to files.
    for_folders: Option<Choice>,
    for_files: Option<Choice>,
    errors: Vec<String>,
}

impl<'a> Run<'a> {
    fn new(
        status: &'a Mutex<Status>,
        stop: &'a AtomicBool,
        ctx: &'a egui::Context,
        answers: &'a Receiver<(Choice, bool)>,
    ) -> Self {
        Run { status, stop, ctx, answers, done: 0, total: 0, for_folders: None, for_files: None, errors: Vec::new() }
    }

    /// Runs `op`: what it placed, and what went wrong.
    fn op(&mut self, op: Op) -> (Vec<PathBuf>, Vec<String>) {
        let mut placed = Vec::new();
        match op {
            Op::Copy { sources, into } => {
                self.total = sources.iter().map(|s| size(s)).sum();
                for source in sources {
                    self.place(&source, &into, false, &mut placed);
                    if self.stopped() {
                        break;
                    }
                }
            }
            Op::Move { sources, into } => {
                for source in sources {
                    self.place(&source, &into, true, &mut placed);
                    if self.stopped() {
                        break;
                    }
                }
            }
            Op::Trash(paths) => {
                for path in paths {
                    if let Err(e) = trash::delete(&path) {
                        self.errors.push(format!("{}: {e}", name(&path)));
                    }
                }
            }
            Op::Restore(items) => {
                for item in items {
                    let to = item.original_parent.join(&item.name);
                    match trash::os_limited::restore_all([item]) {
                        Ok(()) => placed.push(to),
                        Err(e) => self.errors.push(restore_error(e)),
                    }
                }
            }
            Op::Purge(items) => {
                if let Err(e) = trash::os_limited::purge_all(items) {
                    self.errors.push(e.to_string());
                }
            }
        }
        if self.stopped() {
            self.errors.push("Stopped".into());
        }
        (placed, std::mem::take(&mut self.errors))
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Copies or moves `source` into `into`, asking when its name is taken
    /// there; what it placed goes in `placed`, what went wrong in the errors.
    fn place(&mut self, source: &Path, into: &Path, moving: bool, placed: &mut Vec<PathBuf>) {
        match self.place_one(source, into, moving) {
            Ok(Some(to)) => placed.push(to),
            Ok(None) => {}
            Err(e) => self.errors.push(format!("{}: {e}", name(source))),
        }
    }

    fn place_one(&mut self, source: &Path, into: &Path, moving: bool) -> std::io::Result<Option<PathBuf>> {
        refuse_into_itself(source, into)?;
        if moving && source.parent() == Some(into) {
            return Ok(Some(source.to_path_buf()));
        }
        let name = name(source);
        let mut to = into.join(&name);
        if to == source {
            // A copy beside itself.
            to = free_name(into, &name);
        } else if std::fs::symlink_metadata(&to).is_ok() {
            let conflict = Conflict::new(source, &to);
            let folders = conflict.folders;
            match self.ask(conflict) {
                Choice::Skip | Choice::Cancel => {
                    if !moving {
                        self.done += size(source);
                        self.report();
                    }
                    return Ok(None);
                }
                Choice::Merge if folders => {
                    self.merge(source, &to, moving)?;
                    return Ok(Some(to));
                }
                Choice::Merge | Choice::Replace => {
                    if source.starts_with(&to) {
                        return Err(std::io::Error::other("can't replace a folder it's in"));
                    }
                    trash::delete(&to)
                        .map_err(|e| std::io::Error::other(format!("couldn't put the old one in the Trash: {e}")))?;
                }
                Choice::KeepBoth => to = free_name(into, &name),
            }
        }
        if moving {
            self.move_to(source, &to)?;
        } else {
            self.copy(source, &to)?;
        }
        Ok(Some(to))
    }

    /// Puts what's in the folder `source` into the folder `into`, asking
    /// about each name taken; a move then removes `source` if it's empty.
    fn merge(&mut self, source: &Path, into: &Path, moving: bool) -> std::io::Result<()> {
        let mut placed = Vec::new();
        for entry in std::fs::read_dir(source)? {
            self.place(&entry?.path(), into, moving, &mut placed);
            if self.stopped() {
                return Ok(());
            }
        }
        if moving {
            // What was skipped stays, and the folder with it.
            let _ = std::fs::remove_dir(source);
        }
        Ok(())
    }

    /// Renames where it can; across disks, copies and then removes.
    fn move_to(&mut self, source: &Path, to: &Path) -> std::io::Result<()> {
        match std::fs::rename(source, to) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => {
                self.total += size(source);
                self.copy(source, to)?;
                if self.stopped() {
                    return Ok(());
                }
                if std::fs::symlink_metadata(source)?.is_dir() {
                    std::fs::remove_dir_all(source)
                } else {
                    std::fs::remove_file(source)
                }
            }
            Err(e) => Err(e),
        }
    }

    /// What to do about `conflict`: an answer given for all its kind, or
    /// the window's, waited for until it comes or the operation is stopped.
    fn ask(&mut self, conflict: Conflict) -> Choice {
        let folders = conflict.folders;
        if let Some(choice) = if folders { self.for_folders } else { self.for_files } {
            return choice;
        }
        // An answer that came too late for an earlier question.
        while self.answers.try_recv().is_ok() {}
        lock(self.status).conflict = Some(conflict);
        self.ctx.request_repaint();
        let (choice, all) = loop {
            if self.stopped() {
                break (Choice::Cancel, false);
            }
            match self.answers.recv_timeout(Duration::from_millis(100)) {
                Ok(answer) => break answer,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break (Choice::Cancel, false),
            }
        };
        lock(self.status).conflict = None;
        self.ctx.request_repaint();
        if choice == Choice::Cancel {
            self.stop.store(true, Ordering::Relaxed);
        } else if all {
            *if folders { &mut self.for_folders } else { &mut self.for_files } = Some(choice);
        }
        choice
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

/// A folder that can be merged: not a link, and not an app (ADR-0002),
/// which is one item.
fn is_folder(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) && !patina_apps::is_app_folder(path)
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
        answering(op, &[])
    }

    /// Runs `op`, giving `answers` in turn to the conflicts it shows, as
    /// the window does; what each was about goes in the conflicts.
    fn answering(op: Op, answers: &[(Choice, bool)]) -> (Vec<PathBuf>, Vec<String>) {
        let (status, stop, ctx) = (Mutex::new(Status::default()), AtomicBool::new(false), egui::Context::default());
        let (send, receive) = channel();
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let mut answers = answers.iter();
                let mut unanswered = 0;
                while !done.load(Ordering::Relaxed) {
                    if lock(&status).conflict.take().is_some() {
                        let answer = answers.next().copied().unwrap_or_else(|| {
                            unanswered += 1;
                            (Choice::Cancel, false)
                        });
                        send.send(answer).unwrap();
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert_eq!(unanswered, 0, "asked more than answered");
                assert!(answers.next().is_none(), "answered more than asked");
            });
            let result = Run::new(&status, &stop, &ctx, &receive).op(op);
            done.store(true, Ordering::Relaxed);
            result
        })
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

        // Again: kept both, the copy gets a free name.
        let copy = Op::Copy { sources: vec![d.join("src/App.app")], into: d.join("dest") };
        let (placed, _) = answering(copy, &[(Choice::KeepBoth, false)]);
        assert_eq!(placed, [d.join("dest/App copy.app")]);

        let move_ = Op::Move { sources: vec![d.join("src/App.app")], into: d.join("dest") };
        let (placed, errors) = answering(move_, &[(Choice::KeepBoth, false)]);
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

    #[test]
    fn copies_beside_itself_without_asking() {
        let dir = Dir::new("beside");
        let d = &dir.0;
        std::fs::write(d.join("a.txt"), "a").unwrap();
        let (placed, errors) = run(Op::Copy { sources: vec![d.join("a.txt")], into: d.to_path_buf() });
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(placed, [d.join("a copy.txt")]);
    }

    /// src/x/{a, b, sub/c} going where dest/x/{a, sub/d} is.
    fn two_folders(d: &Path) {
        for (file, text) in
            [("src/x/a", "new a"), ("src/x/b", "b"), ("src/x/sub/c", "c"), ("dest/x/a", "old a"), ("dest/x/sub/d", "d")]
        {
            std::fs::create_dir_all(d.join(file).parent().unwrap()).unwrap();
            std::fs::write(d.join(file), text).unwrap();
        }
    }

    fn read(path: PathBuf) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn merges_folders_asking_about_files() {
        let dir = Dir::new("merge");
        let d = &dir.0;
        two_folders(d);
        let copy = Op::Copy { sources: vec![d.join("src/x")], into: d.join("dest") };
        // Merging x merges sub too; a is skipped.
        let (placed, errors) = answering(copy, &[(Choice::Merge, true), (Choice::Skip, false)]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(placed, [d.join("dest/x")]);
        assert_eq!(read(d.join("dest/x/a")), "old a");
        assert_eq!(read(d.join("dest/x/b")), "b");
        assert_eq!(read(d.join("dest/x/sub/c")), "c");
        assert_eq!(read(d.join("dest/x/sub/d")), "d");
        assert!(d.join("src/x/a").exists());

        // Again, keeping both of what's taken.
        let copy = Op::Copy { sources: vec![d.join("src/x")], into: d.join("dest") };
        let (_, errors) = answering(copy, &[(Choice::Merge, true), (Choice::KeepBoth, true)]);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(read(d.join("dest/x/a copy")), "new a");
        assert_eq!(read(d.join("dest/x/b copy")), "b");
        assert_eq!(read(d.join("dest/x/sub/c copy")), "c");
    }

    #[test]
    fn a_merging_move_leaves_what_it_skipped() {
        let dir = Dir::new("merge-move");
        let d = &dir.0;
        two_folders(d);
        std::fs::write(d.join("dest/x/b"), "old b").unwrap();
        let move_ = Op::Move { sources: vec![d.join("src/x")], into: d.join("dest") };
        // Merge x and sub; skip one of a and b, keep both of the other.
        // (Replacing would use the real Trash.)
        let answers = [(Choice::Merge, true), (Choice::Skip, false), (Choice::KeepBoth, false)];
        let (placed, errors) = answering(move_, &answers);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(placed, [d.join("dest/x")]);
        assert_eq!(read(d.join("dest/x/sub/c")), "c");
        assert!(!d.join("src/x/sub").exists());
        let left: Vec<_> = std::fs::read_dir(d.join("src/x")).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left.len(), 1, "{left:?}");
        let kept = if left[0] == "a" { "b" } else { "a" };
        assert!(d.join(format!("dest/x/{kept} copy")).exists());
    }

    #[test]
    fn apps_arent_merged() {
        let dir = Dir::new("apps");
        let d = &dir.0;
        assert!(!is_folder(&d.join("missing")));
        std::fs::create_dir_all(d.join("Tool.app")).unwrap();
        std::fs::create_dir_all(d.join("plain")).unwrap();
        assert!(!is_folder(&d.join("Tool.app")));
        assert!(is_folder(&d.join("plain")));
    }

    #[test]
    fn cancelling_stops_the_rest() {
        let dir = Dir::new("cancel");
        let d = &dir.0;
        two_folders(d);
        let copy = Op::Copy { sources: vec![d.join("src/x"), d.join("src/x/b")], into: d.join("dest") };
        let (placed, errors) = answering(copy, &[(Choice::Cancel, false)]);
        assert!(placed.is_empty());
        assert_eq!(errors, ["Stopped"]);
        assert!(!d.join("dest/b").exists());
    }
}
