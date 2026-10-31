//! What's in a folder, or in the trash: read on a thread, since a folder on
//! a slow disk or a network mount mustn't stop the window, then sorted.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::mime::{self, Mime};

/// Where a tab is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Location {
    Folder(PathBuf),
    Trash,
    /// Servers on the network, or a server's shares, by GVfs URI
    /// (`network:///`, `smb://host/`), listed by `network`.
    Network(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Folder,
    /// A `Name.app` folder: one item, opened by running it.
    App,
    File,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub path: PathBuf,
    /// As shown: an app without `.app`, as a Mac shows it.
    pub name: String,
    pub kind: Kind,
    /// Bytes for a file; the number of items for a folder.
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// When it was made and last opened, where the filesystem says.
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub hidden: bool,
    pub symlink: bool,
    /// The file's type, if its name says.
    pub mime: Option<String>,
    /// "Folder", "Application", or the type's description.
    pub describe: String,
    /// Executable, so it gets the executable's icon when it has no type.
    pub executable: bool,
    /// An executable script, starting `#!`: opening runs it (ADR-0011).
    pub script: bool,
    /// In the trash: where it was, and the item to restore or purge.
    pub trashed: Option<trash::TrashItem>,
    /// A server or share on the network, whose path is its URI.
    pub remote: Option<Remote>,
}

#[derive(Debug, Clone)]
pub struct Remote {
    pub uri: String,
    /// Its theme icon names, the first found wins.
    pub icon: Vec<String>,
}

impl Entry {
    /// The file's name on disk, `.app` and all.
    pub fn file_name(&self) -> String {
        self.path.file_name().map_or_else(|| self.name.clone(), |n| n.to_string_lossy().into_owned())
    }

    /// Whether opening it goes into it.
    pub fn is_folder(&self) -> bool {
        self.kind == Kind::Folder && self.trashed.is_none() && self.remote.is_none()
    }
}

/// Reads `location`.
pub fn read(location: &Location, mime: &Mime) -> Result<Vec<Entry>, String> {
    match location {
        Location::Folder(dir) => read_dir(dir, mime).map_err(|e| e.to_string()),
        Location::Trash => read_trash(mime),
        Location::Network(_) => Err("The network is listed by GVfs".into()),
    }
}

fn read_dir(dir: &Path, mime: &Mime) -> std::io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    for item in std::fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let path = item.path();
        let symlink = item.file_type().is_ok_and(|t| t.is_symlink());
        // A link is shown as what it points to; a broken one as a file.
        let meta = std::fs::metadata(&path).or_else(|_| item.metadata());
        let Ok(meta) = meta else { continue };
        entries.push(entry(path, &meta, symlink, mime));
    }
    Ok(entries)
}

pub fn entry(path: PathBuf, meta: &std::fs::Metadata, symlink: bool, mime: &Mime) -> Entry {
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let hidden = file_name.starts_with('.');
    let modified = meta.modified().ok();
    let (created, accessed) = (meta.created().ok(), meta.accessed().ok());
    if meta.is_dir() {
        if patina_apps::is_app_folder(&path) {
            return Entry {
                name: patina_apps::stem(&path).to_string(),
                path,
                kind: Kind::App,
                size: 0,
                modified,
                created,
                accessed,
                hidden,
                symlink,
                mime: None,
                describe: "Application".into(),
                executable: false,
                script: false,
                trashed: None,
                remote: None,
            };
        }
        let size = std::fs::read_dir(&path).map_or(0, |d| d.count() as u64);
        return Entry {
            name: file_name,
            path,
            kind: Kind::Folder,
            size,
            modified,
            created,
            accessed,
            hidden,
            symlink,
            mime: None,
            describe: "Folder".into(),
            executable: false,
            script: false,
            trashed: None,
            remote: None,
        };
    }
    let ty = mime.guess(&file_name).map(str::to_string);
    let executable = mime::executable(meta);
    let script = executable && is_script(&path);
    let describe = match &ty {
        Some(ty) => mime.describe(ty),
        None if executable => "Program".into(),
        None => "File".into(),
    };
    Entry {
        name: file_name,
        path,
        kind: Kind::File,
        size: meta.len(),
        modified,
        created,
        accessed,
        hidden,
        symlink,
        mime: ty,
        describe,
        executable,
        script,
        trashed: None,
        remote: None,
    }
}

/// Whether the file at `path` starts `#!`, as a script does.
fn is_script(path: &Path) -> bool {
    use std::io::Read;
    let mut start = [0; 2];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut start)).is_ok() && &start == b"#!"
}

/// The trash, through the freedesktop trash spec: each item where it lies
/// in the trash's `files` folder, with where it came from.
fn read_trash(mime: &Mime) -> Result<Vec<Entry>, String> {
    let items = trash::os_limited::list().map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .map(|item| {
            let name = item.name.to_string_lossy().into_owned();
            let inside = in_trash(Path::new(&item.id));
            let meta = std::fs::symlink_metadata(&inside).ok();
            let mut entry = match &meta {
                Some(meta) => entry(inside, meta, false, mime),
                None => Entry {
                    path: inside,
                    name: name.clone(),
                    kind: Kind::File,
                    size: 0,
                    modified: None,
                    created: None,
                    accessed: None,
                    hidden: false,
                    symlink: false,
                    mime: None,
                    describe: "File".into(),
                    executable: false,
                    script: false,
                    trashed: None,
                    remote: None,
                },
            };
            if entry.kind == Kind::App {
                entry.name = name.strip_suffix(".app").unwrap_or(&name).to_string();
            } else {
                entry.name = name;
            }
            entry.hidden = false;
            entry.modified =
                SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(item.time_deleted.max(0) as u64));
            entry.trashed = Some(item);
            entry
        })
        .collect())
}

/// Where a trashed item lies, from its `.trashinfo` file (the item's id):
/// `Trash/info/x.trashinfo` keeps `Trash/files/x`.
fn in_trash(info: &Path) -> PathBuf {
    let trash = info.parent().and_then(Path::parent).unwrap_or(Path::new("."));
    let name = info.file_stem().unwrap_or_default();
    trash.join("files").join(name)
}

/// The home trash's `files` folder.
pub fn trash_files() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("Trash/files")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortBy {
    Name,
    Size,
    Modified,
    Created,
    Accessed,
    Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sort {
    pub by: SortBy,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Sort { by: SortBy::Name, descending: false }
    }
}

/// Folders first, then the rest, each in `sort`'s order; names break ties.
pub fn sort(entries: &mut [Entry], sort: Sort) {
    entries.sort_by(|a, b| {
        let folders = (b.kind == Kind::Folder).cmp(&(a.kind == Kind::Folder));
        let by = match sort.by {
            SortBy::Name => Ordering::Equal,
            SortBy::Size => a.size.cmp(&b.size),
            SortBy::Modified => a.modified.cmp(&b.modified),
            SortBy::Created => a.created.cmp(&b.created),
            SortBy::Accessed => a.accessed.cmp(&b.accessed),
            SortBy::Kind => a.describe.cmp(&b.describe),
        };
        let by = by.then_with(|| natural(&a.name, &b.name));
        folders.then(if sort.descending { by.reverse() } else { by })
    });
}

/// Compares names as people read them: case aside, and numbers by value,
/// so "file 9" comes before "file 10".
pub fn natural(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let number = |s: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut digits = String::new();
                    while let Some(c) = s.next_if(|c| c.is_ascii_digit()) {
                        digits.push(c);
                    }
                    let trimmed = digits.trim_start_matches('0').to_string();
                    (trimmed.len(), trimmed)
                };
                let order = number(&mut a).cmp(&number(&mut b));
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                let order = x.to_lowercase().cmp(y.to_lowercase());
                if order != Ordering::Equal {
                    return order;
                }
                a.next();
                b.next();
            }
        }
    }
}

/// A listing that finished, for the tab showing `location`.
pub struct Listed {
    pub location: Location,
    pub entries: Result<Vec<Entry>, String>,
}

/// Reads `location` on a thread and sends the result, waking the window.
pub fn spawn(location: Location, mime: Arc<Mime>, ctx: egui::Context, send: std::sync::mpsc::Sender<Listed>) {
    std::thread::Builder::new()
        .name("listing".into())
        .spawn(move || {
            let entries = read(&location, &mime);
            if send.send(Listed { location, entries }).is_ok() {
                ctx.request_repaint();
            }
        })
        .ok();
}

/// Bytes as people read them: "812 bytes", "4.2 MB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["kB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut size = bytes as f64 / 1000.0;
    let mut unit = 0;
    while size >= 1000.0 && unit < UNITS.len() - 1 {
        size /= 1000.0;
        unit += 1;
    }
    if size < 10.0 { format!("{size:.1} {}", UNITS[unit]) } else { format!("{size:.0} {}", UNITS[unit]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_sort_by_value_and_case_is_ignored() {
        let mut names = vec!["file 10", "File 9", "file 1", "apple", "Banana", "file 01x"];
        names.sort_by(|a, b| natural(a, b));
        assert_eq!(names, ["apple", "Banana", "file 1", "file 01x", "File 9", "file 10"]);
    }

    #[test]
    fn a_trashed_item_lies_beside_its_info() {
        let info = Path::new("/h/.local/share/Trash/info/a.tar.gz.trashinfo");
        assert_eq!(in_trash(info), Path::new("/h/.local/share/Trash/files/a.tar.gz"));
    }

    #[test]
    fn sizes_read_as_people_say_them() {
        assert_eq!(human_size(812), "812 bytes");
        assert_eq!(human_size(4_200_000), "4.2 MB");
        assert_eq!(human_size(52_000), "52 kB");
    }

    #[test]
    fn lists_folders_apps_and_files() {
        let dir = std::env::temp_dir().join(format!("files-listing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Docs")).unwrap();
        std::fs::create_dir_all(dir.join("Tool.app")).unwrap();
        std::fs::write(dir.join("Docs/a"), "").unwrap();
        std::fs::write(dir.join("b.txt"), "hello").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();
        let mut entries = read(&Location::Folder(dir.clone()), &Mime::load()).unwrap();
        sort(&mut entries, Sort::default());
        let shown: Vec<(&str, Kind, bool)> = entries.iter().map(|e| (e.name.as_str(), e.kind, e.hidden)).collect();
        assert_eq!(
            shown,
            [
                ("Docs", Kind::Folder, false),
                (".hidden", Kind::File, true),
                ("b.txt", Kind::File, false),
                ("Tool", Kind::App, false),
            ]
        );
        assert_eq!(entries[0].size, 1, "a folder's size is its item count");
        assert_eq!(entries[2].size, 5);
        assert_eq!(entries[3].file_name(), "Tool.app");
        std::fs::remove_dir_all(&dir).ok();
    }
}
