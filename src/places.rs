//! The sidebar's places: home, the user's folders, Apps and the Trash;
//! then the disks: the whole computer and whatever is mounted for the user.

use std::path::{Path, PathBuf};

use crate::icons::{self, Source};
use crate::listing::Location;

pub struct Place {
    pub name: String,
    pub location: Location,
    pub icon: Source,
}

impl Place {
    fn folder(name: impl Into<String>, path: PathBuf, icon: &[&str]) -> Self {
        Place { name: name.into(), location: Location::Folder(path), icon: icons::theme(icon) }
    }
}

/// A folder the user added to the sidebar.
pub fn favourite(path: &Path) -> Place {
    Place::folder(name(path), path.to_path_buf(), &["folder-favorites", "folder"])
}

/// Home, Apps, the XDG user folders that exist, and the Trash.
pub fn places() -> Vec<Place> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let mut places = vec![Place::folder("Home", home.clone(), &["user-home", "folder"])];
    if let Some(apps) = patina_apps::dir().filter(|d| d.is_dir()) {
        places.push(Place::folder("Apps", apps, &["folder-apps", "folder"]));
    }
    let user = [
        (dirs::desktop_dir(), &["user-desktop", "folder"][..]),
        (dirs::document_dir(), &["folder-documents", "folder"]),
        (dirs::download_dir(), &["folder-download", "folder"]),
        (dirs::audio_dir(), &["folder-music", "folder"]),
        (dirs::picture_dir(), &["folder-pictures", "folder"]),
        (dirs::video_dir(), &["folder-videos", "folder"]),
    ];
    for (dir, icon) in user {
        if let Some(dir) = dir.filter(|d| d.is_dir() && *d != home) {
            places.push(Place::folder(name(&dir), dir, icon));
        }
    }
    places.push(Place { name: "Trash".into(), location: Location::Trash, icon: icons::theme(&["user-trash"]) });
    places
}

/// The computer's root, then each disk mounted where desktops mount them
/// for the user: `/media/$USER` and `/run/media/$USER` on Linux, `/media`
/// on FreeBSD.
pub fn disks() -> Vec<Place> {
    let mut disks = vec![Place::folder("Computer", PathBuf::from("/"), &["computer", "drive-harddisk"])];
    for dir in mount_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut found: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
        found.sort();
        for path in found {
            disks.push(Place::folder(name(&path), path, &["drive-removable-media", "drive-harddisk"]));
        }
    }
    disks
}

/// Where the user's disks are mounted, which the window watches.
pub fn mount_dirs() -> Vec<PathBuf> {
    let user = std::env::var("USER").unwrap_or_default();
    let mut dirs = Vec::new();
    if !user.is_empty() {
        dirs.push(Path::new("/media").join(&user));
        dirs.push(Path::new("/run/media").join(&user));
    }
    if cfg!(target_os = "freebsd") {
        dirs.push(PathBuf::from("/media"));
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

pub fn name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}
