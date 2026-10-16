//! Noticing changes to the folders on screen, so they re-read themselves:
//! inotify on Linux, kqueue on FreeBSD, through notify.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};

use notify::{RecursiveMode, Watcher as _};

pub struct Watcher {
    watcher: Option<notify::RecommendedWatcher>,
    watching: HashSet<PathBuf>,
    changed: Receiver<PathBuf>,
}

impl Watcher {
    pub fn new(ctx: &egui::Context) -> Self {
        let (send, changed) = channel();
        let ctx = ctx.clone();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else { return };
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            for path in event.paths {
                // The folder that changed: the path's parent, or the path
                // itself when it's the folder being watched.
                let _ = send.send(path.clone());
                if let Some(parent) = path.parent() {
                    let _ = send.send(parent.to_path_buf());
                }
            }
            ctx.request_repaint();
        })
        .map_err(|e| eprintln!("files: can't watch folders for changes: {e}"))
        .ok();
        Watcher { watcher, watching: HashSet::new(), changed }
    }

    /// Watches exactly `folders`, adding and dropping watches to match.
    pub fn set(&mut self, folders: HashSet<PathBuf>) {
        let Some(watcher) = &mut self.watcher else { return };
        for gone in self.watching.difference(&folders) {
            let _ = watcher.unwatch(gone);
        }
        for new in folders.difference(&self.watching) {
            let _ = watcher.watch(new, RecursiveMode::NonRecursive);
        }
        self.watching = folders;
    }

    /// The watched folders that changed since last asked.
    pub fn changed(&self) -> HashSet<PathBuf> {
        self.changed.try_iter().filter(|p| self.watching.contains(p)).collect()
    }
}
