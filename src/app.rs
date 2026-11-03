//! The window: Patina's frame with menus and a tab per folder, a toolbar
//! with the path, the places sidebar, the folder as icons or a list, and a
//! status bar. What the widgets ask for is collected as `Action`s and done
//! at the end of the frame.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::text::{CCursor, CCursorRange, LayoutJob};
use egui::{
    Align, Align2, CornerRadius, DragAndDrop, Event, FontId, Id, Key, LayerId, Order, Rect, Response, Sense, Stroke,
    StrokeKind, TextFormat, Ui, UiBuilder, pos2, vec2,
};
use patina::frame::{self, FrameStyle, TitleBar};
use patina::skin::{self, Skin};
use patina::tabs::{self, TabAction, TabStrip};

use crate::chooser::{Answer, Filter, Request};
use crate::config::{Config, View};
use crate::dnd::Over;
use crate::editor;
use crate::glyphs::{self, Glyph};
use crate::icons::{self, Icons, Source};
use crate::listing::{self, Entry, Kind, Listed, Location, SortBy};
use crate::mime::Mime;
use crate::network::{self, Network};
use crate::ops::{self, Choice, Op};
use crate::places::{self, Place};
use crate::preview::{self, Previews};
use crate::properties::{self, Properties};
use crate::uri;
use crate::watch::Watcher;
use crate::zfs;

/// Files' skins folder is `~/.config/files/skins`.
const APP: &str = "files";

/// The icon view's icons, and each item's cell, in points.
const ICON: f32 = 48.0;
const CELL: egui::Vec2 = vec2(104.0, 104.0);
/// The list view's rows and their icons.
const ROW: f32 = 24.0;
const ROW_ICON: f32 = 16.0;
/// The column view's columns, and the preview at their end.
const COLUMN: f32 = 220.0;
const PREVIEW: f32 = 260.0;
const PREVIEW_ICON: f32 = 128.0;
/// The sidebar's icons.
const PLACE_ICON: f32 = 16.0;
/// Quick Look's window: its app id, so the desktop keeps its place apart
/// from the folder windows', and its size the first time.
const QUICK_LOOK_ID: &str = "com.sectorflabs.Files.QuickLook";
const QUICK_LOOK_SIZE: [f32; 2] = [720.0, 540.0];
/// Its icon, for what isn't a picture.
const QUICK_LOOK_ICON: f32 = 128.0;
/// The Properties window's icon.
const PROPERTIES_ICON: f32 = 64.0;
/// How long typed letters keep adding to the name they find.
const TYPE_AHEAD: Duration = Duration::from_millis(1000);

/// What's being dragged: the paths of the selected items.
#[derive(Clone)]
struct Dragged(Vec<PathBuf>);

struct Tab {
    location: Location,
    back: Vec<Location>,
    forward: Vec<Location>,
    entries: Vec<Entry>,
    loaded: bool,
    error: Option<String>,
    selected: HashSet<PathBuf>,
    /// Where shift-click and shift-arrows extend from.
    anchor: Option<PathBuf>,
    /// The item the arrow keys move from.
    cursor: Option<PathBuf>,
    /// To select once the next listing has them: what was just made,
    /// pasted or come up from.
    select_after: Vec<PathBuf>,
    /// To rename once it's listed: a new folder.
    rename_after: Option<PathBuf>,
    scroll_to: Option<PathBuf>,
    filter: String,
    /// While choosing: the file types shown; other files are left out.
    types: Option<Filter>,
}

impl Tab {
    fn new(location: Location) -> Self {
        Tab {
            location,
            back: Vec::new(),
            forward: Vec::new(),
            entries: Vec::new(),
            loaded: false,
            error: None,
            selected: HashSet::new(),
            anchor: None,
            cursor: None,
            select_after: Vec::new(),
            rename_after: None,
            scroll_to: None,
            filter: String::new(),
            types: None,
        }
    }

    fn folder(&self) -> Option<&Path> {
        match &self.location {
            Location::Folder(path) => Some(path),
            Location::Trash | Location::Network(_) => None,
        }
    }

    /// Whether it lists servers or shares, not files.
    fn remote(&self) -> bool {
        matches!(self.location, Location::Network(_))
    }

    fn in_trash(&self) -> bool {
        self.location == Location::Trash
    }

    /// The entries shown: hidden ones only when asked, and only those the
    /// filter matches, as indices into `entries`.
    fn visible(&self, show_hidden: bool) -> Vec<usize> {
        shown(&self.entries, show_hidden, &self.filter, self.types.as_ref())
    }

    /// Selects what `select_after` is waiting for, if it's listed now.
    fn settle(&mut self) {
        let wanted: Vec<PathBuf> =
            self.select_after.iter().filter(|p| self.entries.iter().any(|e| &e.path == *p)).cloned().collect();
        if !wanted.is_empty() {
            self.select_only(wanted);
            self.select_after.clear();
        }
    }

    fn selection(&self) -> Vec<&Entry> {
        self.entries.iter().filter(|e| self.selected.contains(&e.path)).collect()
    }

    fn select_only(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        self.selected = paths.into_iter().collect();
        let first = self.entries.iter().find(|e| self.selected.contains(&e.path)).map(|e| e.path.clone());
        self.anchor = first.clone();
        self.cursor = first.clone();
        self.scroll_to = first;
    }
}

struct Renaming {
    path: PathBuf,
    text: String,
    /// Whether the field has been focused and its name selected.
    started: bool,
}

/// Cut or copied files, waiting for a paste.
struct Clip {
    paths: Vec<PathBuf>,
    cut: bool,
    /// What went on the system clipboard with them, to know a paste is ours.
    text: String,
}

/// A server's question for the user, being answered.
struct Asking {
    ask: network::Ask,
    user: String,
    domain: String,
    password: String,
    anonymous: bool,
    save: network::Save,
    /// Whether a field has been given the keys.
    focused: bool,
}

enum Confirm {
    Purge(Vec<trash::TrashItem>),
    EmptyTrash,
    /// Saving over a file that's there.
    Replace(PathBuf),
}

/// How the window starts: browsing, or choosing for an app (ADR-0004).
pub enum Start {
    Browse(Option<PathBuf>),
    /// The network, a server, or a share to mount, by GVfs URI.
    Remote(String),
    Choose(Request, Arc<Mutex<Option<Answer>>>),
}

/// Choosing files for an app, through the portal.
struct Picking {
    request: Request,
    /// The save field's name.
    name: String,
    /// The file type chosen, an index into the request's filters.
    filter: Option<usize>,
    /// Where the answer goes when the window closes.
    answer: Arc<Mutex<Option<Answer>>>,
    /// Whether the save field has been focused, with its name selected.
    name_started: bool,
}

impl Picking {
    /// The types shown: the chosen filter's, or only folders when choosing
    /// a folder.
    fn types(&self) -> Option<Filter> {
        if self.request.directory {
            return Some(Filter::default());
        }
        self.filter.and_then(|i| self.request.filters.get(i)).cloned()
    }
}

enum Action {
    Open(PathBuf),
    OpenInNewTab(Location),
    AddFavourite(PathBuf),
    RemoveFavourite(PathBuf),
    ShowContents(PathBuf),
    Go(Location),
    Back,
    Forward,
    Up,
    Click {
        path: PathBuf,
        add: bool,
        range: bool,
    },
    SelectOnly(Vec<PathBuf>),
    SelectAll,
    ClearSelection,
    Copy,
    Cut,
    Paste(Option<String>),
    CopyPath,
    Trash,
    Restore,
    Purge,
    EmptyTrash,
    StartRename,
    NewFolder,
    Terminal(PathBuf),
    /// A script opened in the text editor (ADR-0011).
    Edit(PathBuf),
    NewWindow,
    Drop {
        paths: Vec<PathBuf>,
        into: Location,
    },
    Move(isize),
    /// In the column view: makes another column's folder the tab's, with
    /// that item selected.
    Column {
        folder: PathBuf,
        select: Option<PathBuf>,
    },
    /// Right in the column view: into the folder selected, at its first item.
    IntoColumn,
    Tab(TabAction),
    /// A typed URI that isn't a file's: a server to list or a share to mount.
    OpenUri(String),
    /// Mounts a share, then goes into it; with a name for the status bar.
    Mount(String, String),
    Unmount(String),
    ConnectToServer,
    CloseTab,
    Quit,
    Accept,
    Cancel,
    /// While choosing: a typed path, chosen as it is.
    ChoosePath(PathBuf),
    SetView(View),
    ToggleHidden,
    EditLocation,
    FocusFilter,
    /// Space: shows the selected item large in a window of its own, or closes it.
    QuickLook,
    /// Shows the Properties of what's selected, or of the folder.
    Properties,
    CloseProperties,
}

pub struct Files {
    /// Drags out of the window, to other apps, on Wayland (ADR-0006).
    dnd: Option<crate::dnd::Dnd>,
    /// The files egui is dragging are a Wayland drag too.
    dragging_out: bool,
    /// And it was dropped on our own window.
    dropped_here: bool,
    /// Another app's (or Files window's) files are being dragged over us.
    incoming: bool,
    /// The drop just made is a copy, as the dragging app was told (Ctrl).
    drop_copy: bool,
    config: Config,
    skins: Vec<Skin>,
    skin: usize,
    skin_errors: Vec<String>,
    tabs: Vec<Tab>,
    tab: usize,
    mime: Arc<Mime>,
    icons: Icons,
    previews: Previews,
    /// Quick Look is showing the item at the cursor (ADR-0008).
    looking: bool,
    /// The Properties window, while it's open.
    properties: Option<Properties>,
    places: Vec<Place>,
    disks: Vec<Place>,
    /// Folders with their own icon: home, Apps, Documents and the rest.
    special: HashMap<PathBuf, Source>,
    watcher: Watcher,
    listed_send: Sender<Listed>,
    listed: Receiver<Listed>,
    /// Listings being read, and ones that changed again while they were.
    reading: HashSet<Location>,
    /// The folder the status bar's ZFS facts are for, when they were read, and them.
    zfs: Option<(PathBuf, std::time::Instant, Option<zfs::Info>)>,
    zfs_reader: zfs::Reader,
    zfs_asked: Option<(PathBuf, std::time::Instant)>,
    /// The ZFS side panel, opened by a click on the status bar's dataset.
    zfs_details: Option<(String, zfs::Details)>,
    zfs_detail_reader: zfs::DetailReader,
    zfs_detail_asked: Option<(String, std::time::Instant)>,
    stale: HashSet<Location>,
    worker: ops::Worker,
    clip: Option<Clip>,
    renaming: Option<Renaming>,
    location_edit: Option<String>,
    /// Whether the location field has been focused, with its text selected.
    location_started: bool,
    /// Whether the location field starts with its text selected, to type
    /// over, or the cursor at its end, to go on typing.
    location_select: bool,
    network: Network,
    /// The network shares mounted now.
    mounts: Vec<network::Mount>,
    /// Questions from servers, the first one showing.
    asking: Vec<Asking>,
    /// A share being mounted to go into: its URI and name.
    connecting: Option<(String, String)>,
    focus_filter: bool,
    confirm: Option<Confirm>,
    /// The conflict dialog's "for all" box (ADR-0009).
    for_all: bool,
    /// The latest message for the status bar, until the next replaces it.
    message: Option<Message>,
    typed: (String, Instant),
    /// The icon view's columns, for the arrow keys.
    columns: usize,
    /// The toolbar's middle, which the path bar centres on.
    toolbar_middle: f32,
    /// Where the item area's content starts on screen, scrolled.
    origin: egui::Pos2,
    /// A drag on the background: where it started, in content coordinates,
    /// and what was selected before it.
    band: Option<(egui::Pos2, HashSet<PathBuf>)>,
    /// The item area's scroll offset and height, last frame.
    viewport: (f32, f32),
    /// The column view's other folders, read; an error if one can't be.
    column_lists: HashMap<PathBuf, Result<Vec<Entry>, String>>,
    /// The folders it showed last frame, to bring new ones into view.
    shown_columns: Vec<PathBuf>,
    actions: Vec<Action>,
    title: String,
    screenshot: Option<(PathBuf, bool)>,
    picking: Option<Picking>,
}

impl Files {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Start) -> Self {
        patina::appearance::watch(&cc.egui_ctx);
        let ctx = &cc.egui_ctx;
        let config = Config::load();
        let (skins, skin_errors) = load_skins();
        let skin = skins.iter().position(|s| s.name == config.skin).unwrap_or(0);
        let (listed_send, listed) = channel();
        let network = Network::new(ctx, listed_send.clone());

        let mut remote = None;
        let (start, picking) = match start {
            Start::Browse(path) => (path, None),
            Start::Remote(uri) => {
                remote = Some(uri);
                (None, None)
            }
            Start::Choose(request, answer) => {
                // Where the app says, else where the last choice was made.
                let folder = request.start().or_else(|| config.chooser_folder.clone().filter(|f| f.is_dir()));
                let picking = Picking {
                    name: request.start_name(),
                    filter: request.current_filter.or((!request.filters.is_empty()).then_some(0)),
                    request,
                    answer,
                    name_started: false,
                };
                (folder, Some(picking))
            }
        };
        // A file to start at opens its folder with it selected.
        let (location, select) = match start {
            Some(path) if path.is_dir() => (Location::Folder(path), None),
            Some(path) if path.exists() => {
                let parent = path.parent().map_or_else(|| PathBuf::from("/"), Path::to_path_buf);
                (Location::Folder(parent), Some(path))
            }
            _ => (Location::Folder(dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))), None),
        };
        let mut first = Tab::new(location);
        first.select_after.extend(select);
        first.types = picking.as_ref().and_then(Picking::types);

        let places = places::places();
        let mut special: HashMap<PathBuf, Source> = places
            .iter()
            .filter_map(|p| match &p.location {
                Location::Folder(path) => Some((path.clone(), p.icon.clone())),
                Location::Trash | Location::Network(_) => None,
            })
            .collect();
        special.remove(Path::new("/"));

        let dnd = {
            use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
            match (cc.display_handle(), cc.window_handle()) {
                (Ok(d), Ok(w)) => crate::dnd::Dnd::new(ctx, d.as_raw(), w.as_raw()),
                _ => None,
            }
        };
        let mut files = Files {
            dnd,
            dragging_out: false,
            dropped_here: false,
            incoming: false,
            drop_copy: false,
            config,
            skins,
            skin,
            skin_errors,
            tabs: vec![first],
            tab: 0,
            mime: Arc::new(Mime::load()),
            icons: Icons::new(ctx),
            previews: Previews::new(ctx),
            looking: false,
            properties: None,
            places,
            disks: places::disks(),
            special,
            watcher: Watcher::new(ctx),
            listed_send,
            listed,
            reading: HashSet::new(),
            zfs: None,
            zfs_reader: zfs::Reader::new(),
            zfs_asked: None,
            zfs_details: None,
            zfs_detail_reader: zfs::DetailReader::new(),
            zfs_detail_asked: None,
            stale: HashSet::new(),
            worker: ops::Worker::new(ctx),
            clip: None,
            renaming: None,
            location_edit: None,
            location_started: false,
            location_select: true,
            network,
            mounts: Vec::new(),
            asking: Vec::new(),
            connecting: None,
            focus_filter: false,
            confirm: None,
            for_all: false,
            message: None,
            typed: (String::new(), Instant::now()),
            columns: 1,
            toolbar_middle: 0.0,
            origin: egui::Pos2::ZERO,
            band: None,
            viewport: (0.0, 0.0),
            column_lists: HashMap::new(),
            shown_columns: Vec::new(),
            actions: Vec::new(),
            title: String::new(),
            screenshot: std::env::var_os("FILES_SCREENSHOT").map(|p| (PathBuf::from(p), false)),
            picking,
        };
        files.apply_skin(ctx);
        if files.config.system_title_bar {
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
        }
        match remote {
            Some(uri) if network::is_listing(&uri) => files.tabs[0] = Tab::new(Location::Network(uri)),
            Some(uri) => files.actions.push(Action::OpenUri(uri)),
            None => {}
        }
        files.load(files.tabs[0].location.clone(), ctx);
        files
    }

    fn current(&self) -> &Tab {
        &self.tabs[self.tab]
    }

    fn current_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.tab]
    }

    fn apply_skin(&self, ctx: &egui::Context) {
        let skin = &self.skins[self.skin];
        skin.apply(ctx);
        if skin.follows_desktop() {
            ctx.set_theme(self.config.theme());
        }
    }

    /// Reads `location` again for every tab showing it.
    fn load(&mut self, location: Location, ctx: &egui::Context) {
        if self.reading.contains(&location) {
            self.stale.insert(location);
            return;
        }
        self.reading.insert(location.clone());
        match &location {
            Location::Network(uri) => self.network.list(uri),
            _ => listing::spawn(location, self.mime.clone(), ctx.clone(), self.listed_send.clone()),
        }
    }

    /// Takes in finished listings, file changes and finished operations.
    /// Reads the ZFS facts of the folder shown when it changes, and again
    /// every few seconds, since the space moves.
    fn poll_zfs(&mut self, ctx: &egui::Context) {
        for (folder, info) in self.zfs_reader.poll() {
            if self.current().folder() == Some(folder.as_path()) {
                self.zfs = Some((folder, std::time::Instant::now(), info));
            }
        }
        let Some(folder) = self.current().folder().map(Path::to_path_buf) else {
            self.zfs = None;
            return;
        };
        const EVERY: std::time::Duration = std::time::Duration::from_secs(10);
        let asked = self.zfs_asked.as_ref().filter(|(f, _)| *f == folder).map(|(_, at)| *at);
        if asked.is_none_or(|at| at.elapsed() > EVERY) {
            if asked.is_none() {
                self.zfs = None;
            }
            self.zfs_asked = Some((folder.clone(), std::time::Instant::now()));
            self.zfs_reader.start(folder, ctx);
        }
        ctx.request_repaint_after(EVERY);
        self.poll_zfs_details(ctx, EVERY);
    }

    /// The side panel's facts: read while it is open, for the dataset of the
    /// folder shown, and again every few seconds.
    fn poll_zfs_details(&mut self, ctx: &egui::Context, every: std::time::Duration) {
        for (dataset, details) in self.zfs_detail_reader.poll() {
            if matches!(&self.zfs, Some((_, _, Some(i))) if i.dataset == dataset) {
                self.zfs_details = Some((dataset, details));
            }
        }
        let dataset = match &self.zfs {
            Some((_, _, Some(info))) if self.config.zfs_panel => info.dataset.clone(),
            _ => {
                self.zfs_detail_asked = None;
                return;
            }
        };
        let asked = self.zfs_detail_asked.as_ref().filter(|(d, _)| *d == dataset).map(|(_, at)| *at);
        if asked.is_none_or(|at| at.elapsed() > every) {
            if asked.is_none() {
                self.zfs_details = None;
            }
            self.zfs_detail_asked = Some((dataset.clone(), std::time::Instant::now()));
            self.zfs_detail_reader.start(dataset, ctx);
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        self.poll_zfs(ctx);
        self.icons.poll(ctx);
        self.previews.poll(ctx);
        let wanted = self.wanted_columns();
        while let Ok(Listed { location, entries }) = self.listed.try_recv() {
            self.reading.remove(&location);
            if self.stale.remove(&location) {
                self.load(location.clone(), ctx);
            }
            if let Location::Folder(folder) = &location
                && wanted.contains(folder)
            {
                let mut entries = entries.clone();
                if let Ok(entries) = &mut entries {
                    listing::sort(entries, self.config.sort);
                }
                self.column_lists.insert(folder.clone(), entries);
            }
            let renaming = self.renaming.is_some();
            let mut rename = None;
            for tab in self.tabs.iter_mut().filter(|t| t.location == location) {
                tab.loaded = true;
                match &entries {
                    Ok(entries) => {
                        tab.error = None;
                        tab.entries = entries.clone();
                        listing::sort(&mut tab.entries, self.config.sort);
                    }
                    Err(e) => {
                        tab.error = Some(e.clone());
                        tab.entries.clear();
                    }
                }
                let present: HashSet<PathBuf> = tab.entries.iter().map(|e| e.path.clone()).collect();
                tab.selected.retain(|p| present.contains(p));
                tab.settle();
                if let Some(path) = tab.rename_after.take_if(|p| present.contains(p))
                    && !renaming
                {
                    tab.select_only([path.clone()]);
                    rename = Some(path);
                }
            }
            if let Some(path) = rename {
                self.start_rename(&path);
            }
        }

        for event in self.network.events() {
            self.network_event(event, ctx);
        }

        for changed in self.watcher.changed() {
            if changed == listing::trash_files() || changed.ends_with("Trash/info") {
                self.load(Location::Trash, ctx);
            } else if places::mount_dirs().contains(&changed) {
                self.disks = places::disks();
            } else {
                self.load(Location::Folder(changed), ctx);
            }
        }

        let (placed, errors) = {
            let mut status = self.worker.status.lock().unwrap_or_else(|e| e.into_inner());
            if status.running.is_none() {
                (std::mem::take(&mut status.placed), std::mem::take(&mut status.errors))
            } else {
                (Vec::new(), Vec::new())
            }
        };
        if let Some(first) = errors.first() {
            let more = errors.len() - 1;
            let text = if more > 0 { format!("{first} (and {more} more)") } else { first.clone() };
            let mut message = Message::error(text);
            if more > 0 {
                message.detail = errors;
            }
            self.message = Some(message);
        }
        if !placed.is_empty() {
            let tab = self.current_mut();
            if let Some(folder) = tab.folder().map(Path::to_path_buf) {
                tab.select_after = placed.into_iter().filter(|p| p.parent() == Some(&folder)).collect();
                let location = tab.location.clone();
                self.load(location, ctx);
            }
        }

        let mut folders: HashSet<PathBuf> =
            self.tabs.iter().filter_map(|t| t.folder().map(Path::to_path_buf)).collect();
        if self.tabs.iter().any(Tab::in_trash) {
            let files = listing::trash_files();
            if let Some(info) = files.parent().map(|t| t.join("info")) {
                folders.insert(info);
            }
            folders.insert(files);
        }
        // The other columns' folders: read when they're first shown, and
        // forgotten when they're not.
        self.column_lists.retain(|folder, _| wanted.contains(folder));
        for folder in wanted {
            let location = Location::Folder(folder.clone());
            if !self.column_lists.contains_key(&folder) && !self.reading.contains(&location) {
                self.load(location, ctx);
            }
            folders.insert(folder);
        }
        folders.extend(places::mount_dirs());
        self.watcher.set(folders);
    }

    /// Whether the folder is shown in columns: the Trash and the network
    /// are always a list.
    fn in_columns(&self) -> bool {
        self.config.view == View::Columns && self.current().folder().is_some()
    }

    /// The folders left of this one in the column view, from where its path
    /// starts (Home, the computer or a share), as the path bar has them.
    fn column_chain(&self) -> Vec<PathBuf> {
        let mut chain: Vec<PathBuf> = crumbs(&self.current().location, &self.mounts)
            .into_iter()
            .filter_map(|(_, location)| match location {
                Location::Folder(path) => Some(path),
                Location::Trash | Location::Network(_) => None,
            })
            .collect();
        chain.pop();
        chain
    }

    /// The one folder selected, whose items are the column on the right.
    fn selected_folder(&self) -> Option<PathBuf> {
        match self.current().selection()[..] {
            [entry] if entry.is_folder() => Some(entry.path.clone()),
            _ => None,
        }
    }

    /// The folders the column view lists apart from the tab's own: those to
    /// its left, itself (for when it's one of those), and the one selected.
    fn wanted_columns(&self) -> HashSet<PathBuf> {
        if !self.in_columns() {
            return HashSet::new();
        }
        let mut wanted: HashSet<PathBuf> = self.column_chain().into_iter().collect();
        wanted.extend(self.current().folder().map(Path::to_path_buf));
        wanted.extend(self.selected_folder());
        wanted
    }

    fn network_event(&mut self, event: network::Event, ctx: &egui::Context) {
        match event {
            network::Event::Mounted { uri, result } => {
                let name = match self.connecting.take_if(|(u, _)| *u == uri) {
                    Some((_, name)) => name,
                    None => return,
                };
                match result {
                    Ok(path) => self.go(Location::Folder(path), ctx),
                    Err(Some(e)) => self.message = Some(Message::error(format!("Couldn't connect to {name}: {e}"))),
                    Err(None) => {}
                }
            }
            network::Event::Unmounted { uri, result: Err(Some(e)) } => {
                let name = self.mounts.iter().find(|m| m.uri == uri).map_or(uri.clone(), |m| m.name.clone());
                self.message = Some(Message::error(format!("Couldn't unmount {name}: {e}")));
            }
            network::Event::Unmounted { .. } => {}
            network::Event::Mounts(mounts) => {
                // Tabs in a share that's gone go to its server.
                let gone: Vec<network::Mount> =
                    self.mounts.iter().filter(|m| !mounts.iter().any(|n| n.path == m.path)).cloned().collect();
                self.mounts = mounts;
                for mount in gone {
                    for i in 0..self.tabs.len() {
                        if self.tabs[i].folder().is_some_and(|f| f.starts_with(&mount.path)) {
                            let shown = std::mem::replace(&mut self.tab, i);
                            self.go(Location::Network(network::server_of(&mount.uri)), ctx);
                            self.tab = shown;
                        }
                    }
                }
            }
            network::Event::Ask(ask) => {
                let (user, domain) = match &ask.kind {
                    network::AskKind::Password { user, domain, .. } => (user.clone(), domain.clone()),
                    network::AskKind::Question(_) => Default::default(),
                };
                self.asking.push(Asking {
                    ask,
                    user,
                    domain,
                    password: String::new(),
                    anonymous: false,
                    save: network::Save::ForSession,
                    focused: false,
                });
            }
            network::Event::Withdrawn(id) => self.asking.retain(|a| a.ask.id != id),
        }
    }

    /// Where Up goes: the enclosing folder; from a share, its server; from
    /// a server, the network.
    fn up_from(&self, location: &Location) -> Option<Location> {
        match location {
            Location::Trash => None,
            Location::Network(uri) if uri == network::ROOT => None,
            Location::Network(_) => Some(Location::Network(network::ROOT.into())),
            Location::Folder(path) => match self.mounts.iter().find(|m| m.path == *path) {
                Some(mount) => Some(Location::Network(network::server_of(&mount.uri))),
                None => path.parent().map(|p| Location::Folder(p.to_path_buf())),
            },
        }
    }

    fn icon_source(&self, entry: &Entry) -> Source {
        if let Some(remote) = &entry.remote {
            return Source::Theme(remote.icon.clone());
        }
        match entry.kind {
            Kind::Folder => self.special.get(&entry.path).cloned().unwrap_or_else(|| icons::theme(&["folder"])),
            Kind::App => Source::App(entry.path.clone()),
            Kind::File => match &entry.mime {
                Some(ty) if icons::has_thumbnail(ty) => {
                    Source::Thumbnail(entry.path.clone(), entry.modified, self.mime.icon_names(ty))
                }
                Some(ty) => Source::Theme(self.mime.icon_names(ty)),
                None if entry.executable => icons::theme(&["application-x-executable", "text-x-generic"]),
                None => icons::theme(&["text-x-generic"]),
            },
        }
    }

    fn title_of(location: &Location, mounts: &[network::Mount]) -> String {
        match location {
            Location::Trash => "Trash".into(),
            Location::Network(uri) => network::host(uri).unwrap_or("Network").to_string(),
            Location::Folder(path) if let Some(mount) = mounts.iter().find(|m| m.path == *path) => mount.name.clone(),
            Location::Folder(path) if Some(path) == dirs::home_dir().as_ref() => "Home".into(),
            Location::Folder(path) if path == Path::new("/") => "Computer".into(),
            Location::Folder(path) => places::name(path),
        }
    }

    // ---- Doing what was asked ----

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Open(path) => {
                self.looking = false;
                self.open(&path, ctx);
            }
            Action::QuickLook => self.looking = !self.looking && self.looked_at().is_some(),
            Action::AddFavourite(path) => {
                if !self.config.favourites.contains(&path) {
                    self.config.favourites.push(path);
                    self.config.save();
                }
            }
            Action::RemoveFavourite(path) => {
                self.config.favourites.retain(|p| *p != path);
                self.config.save();
            }
            Action::OpenInNewTab(location) => {
                self.tabs.insert(self.tab + 1, Tab::new(location.clone()));
                self.load(location, ctx);
            }
            Action::ShowContents(path) => self.go(Location::Folder(path), ctx),
            Action::Go(location) => self.go(location, ctx),
            Action::Back => {
                let tab = self.current_mut();
                if let Some(previous) = tab.back.pop() {
                    let here = std::mem::replace(&mut tab.location, previous.clone());
                    tab.forward.push(here);
                    self.arrive(ctx);
                }
            }
            Action::Forward => {
                let tab = self.current_mut();
                if let Some(next) = tab.forward.pop() {
                    let here = std::mem::replace(&mut tab.location, next);
                    tab.back.push(here);
                    self.arrive(ctx);
                }
            }
            Action::Up => {
                let here = self.current().location.clone();
                if let Some(up) = self.up_from(&here) {
                    self.go(up, ctx);
                    if let Location::Folder(folder) = here {
                        let tab = self.current_mut();
                        tab.select_after = vec![folder];
                        // Already listed when it's a column.
                        tab.settle();
                    }
                }
            }
            Action::Click { path, add, range } => self.click(path, add, range),
            Action::SelectOnly(paths) => self.current_mut().select_only(paths),
            Action::SelectAll => {
                let show_hidden = self.config.show_hidden;
                let tab = self.current_mut();
                let all: Vec<PathBuf> = tab.visible(show_hidden).iter().map(|&i| tab.entries[i].path.clone()).collect();
                tab.selected = all.into_iter().collect();
            }
            Action::ClearSelection => self.current_mut().selected.clear(),
            Action::Copy | Action::Cut => {
                let cut = matches!(action, Action::Cut);
                let paths: Vec<PathBuf> = self.current().selection().iter().map(|e| e.path.clone()).collect();
                if !paths.is_empty() && self.current().folder().is_some() {
                    let text = uri::clipboard_text(&paths, cut);
                    ctx.copy_text(text.clone());
                    self.clip = Some(Clip { paths, cut, text });
                }
            }
            Action::Paste(text) => self.paste(text),
            Action::CopyPath => {
                let paths: Vec<String> =
                    self.current().selection().iter().map(|e| e.path.display().to_string()).collect();
                if !paths.is_empty() {
                    ctx.copy_text(paths.join("\n"));
                }
            }
            Action::Trash => {
                let paths: Vec<PathBuf> = self.current().selection().iter().map(|e| e.path.clone()).collect();
                if self.current().in_trash() {
                    self.apply(Action::Purge, ctx);
                } else if !paths.is_empty() && self.current().folder().is_some() {
                    self.select_neighbour();
                    self.worker.start(Op::Trash(paths));
                }
            }
            Action::Restore => {
                let items: Vec<trash::TrashItem> =
                    self.current().selection().iter().filter_map(|e| e.trashed.clone()).collect();
                if !items.is_empty() {
                    self.worker.start(Op::Restore(items));
                }
            }
            Action::Purge => {
                let items: Vec<trash::TrashItem> =
                    self.current().selection().iter().filter_map(|e| e.trashed.clone()).collect();
                if !items.is_empty() {
                    self.confirm = Some(Confirm::Purge(items));
                }
            }
            Action::EmptyTrash => self.confirm = Some(Confirm::EmptyTrash),
            Action::StartRename => {
                let selection = self.current().selection();
                if let [entry] = selection[..] {
                    let path = entry.path.clone();
                    self.start_rename(&path);
                }
            }
            Action::NewFolder => {
                if let Some(folder) = self.current().folder().map(Path::to_path_buf) {
                    match ops::new_folder(&folder) {
                        Ok(path) => {
                            self.current_mut().rename_after = Some(path);
                            self.load(Location::Folder(folder), ctx);
                        }
                        Err(e) => self.message = Some(Message::error(format!("Can't make a folder here: {e}"))),
                    }
                }
            }
            Action::Terminal(dir) => {
                if let Err(e) = spawn_in(&patina_apps::terminal(), &[], &dir) {
                    self.message = Some(Message::error(format!("Can't open a terminal: {e}")));
                }
            }
            Action::Edit(path) => {
                let line = editor::command(&path);
                let started = line.and_then(|line| {
                    let dir = path.parent().unwrap_or(Path::new("/"));
                    spawn_in(&line[0], &line[1..], dir).map_err(|e| format!("{}: {e}", line[0]))
                });
                if let Err(e) = started {
                    self.message = Some(Message::error(format!("Can't edit {}: {e}", places::name(&path))));
                }
            }
            Action::NewWindow => {
                let here = self.current().folder().map(Path::to_path_buf);
                let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("files"));
                let args: Vec<String> = here.iter().map(|p| p.display().to_string()).collect();
                if let Err(e) = spawn_in(&exe.to_string_lossy(), &args, &dirs::home_dir().unwrap_or_default()) {
                    self.message = Some(Message::error(format!("Can't open a window: {e}")));
                }
            }
            Action::Drop { paths, into } => {
                let copy = ctx.input(|i| i.modifiers.ctrl) || std::mem::take(&mut self.drop_copy);
                self.drop_into(paths, into, copy);
            }
            Action::Move(step) => self.step(step, ctx.input(|i| i.modifiers.shift)),
            Action::Column { folder, select } => {
                self.go(Location::Folder(folder), ctx);
                match select {
                    Some(path) => {
                        self.click(path.clone(), false, false);
                        self.current_mut().scroll_to = Some(path);
                    }
                    None => self.current_mut().selected.clear(),
                }
            }
            Action::IntoColumn => {
                let Some(folder) = self.selected_folder() else { return };
                let show_hidden = self.config.show_hidden;
                let first = match self.column_lists.get(&folder) {
                    Some(Ok(entries)) => shown(entries, show_hidden, "", self.current().types.as_ref())
                        .first()
                        .map(|&i| entries[i].path.clone()),
                    _ => None,
                };
                self.apply(Action::Column { folder, select: first }, ctx);
            }
            Action::Tab(action) => self.tab_action(action, ctx),
            Action::OpenUri(uri) => {
                if network::is_listing(&uri) {
                    self.go(Location::Network(uri), ctx);
                } else {
                    let name = uri.split_once("://").map_or(uri.as_str(), |(_, rest)| rest.trim_end_matches('/'));
                    self.apply(Action::Mount(uri.clone(), name.to_string()), ctx);
                }
            }
            Action::Mount(uri, name) => {
                self.network.mount(&uri);
                self.connecting = Some((uri, name));
            }
            Action::Unmount(uri) => self.network.unmount(&uri),
            Action::ConnectToServer => {
                self.location_started = false;
                self.location_select = false;
                self.location_edit = Some("smb://".into());
            }
            Action::CloseTab => {
                if self.tabs.len() > 1 {
                    self.tab_action(TabAction::Close(self.tab), ctx);
                } else {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::Accept => self.accept(ctx),
            Action::Cancel => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::ChoosePath(path) => self.choose_path(path, ctx),
            Action::SetView(view) => {
                self.config.view = view;
                self.config.save();
            }
            Action::ToggleHidden => {
                self.config.show_hidden = !self.config.show_hidden;
                self.config.save();
            }
            Action::EditLocation => {
                self.location_started = false;
                self.location_select = true;
                self.location_edit = Some(match &self.current().location {
                    Location::Folder(path) => path.display().to_string(),
                    Location::Trash => String::new(),
                    Location::Network(uri) => uri.clone(),
                });
            }
            Action::FocusFilter => self.focus_filter = true,
            Action::Properties => {
                let tab = self.current();
                let mut paths: Vec<PathBuf> = tab.selection().iter().map(|e| e.path.clone()).collect();
                if paths.is_empty() {
                    paths.extend(tab.folder().map(Path::to_path_buf));
                }
                if tab.folder().is_some() && !paths.is_empty() {
                    self.properties = Some(Properties::new(paths, self.mime.clone(), ctx));
                }
            }
            Action::CloseProperties => self.properties = None,
        }
    }

    fn go(&mut self, location: Location, ctx: &egui::Context) {
        let tab = self.current_mut();
        if tab.location == location {
            return;
        }
        let here = std::mem::replace(&mut tab.location, location);
        tab.back.push(here);
        tab.forward.clear();
        self.arrive(ctx);
    }

    /// After the tab's location changed: a fresh listing.
    fn arrive(&mut self, ctx: &egui::Context) {
        self.renaming = None;
        self.location_edit = None;
        self.band = None;
        let tab = self.current_mut();
        tab.entries.clear();
        tab.loaded = false;
        tab.error = None;
        tab.selected.clear();
        tab.anchor = None;
        tab.cursor = None;
        tab.select_after.clear();
        tab.filter.clear();
        let location = tab.location.clone();
        // A column's folder is already read, so it's shown at once.
        if let Location::Folder(folder) = &location
            && let Some(Ok(entries)) = self.column_lists.get(folder)
        {
            let tab = &mut self.tabs[self.tab];
            tab.entries = entries.clone();
            tab.loaded = true;
        }
        self.load(location, ctx);
    }

    /// Opens what's at `path`: goes into a folder, runs an app or a script,
    /// and hands a file to the desktop's app for it.
    fn open(&mut self, path: &Path, ctx: &egui::Context) {
        let Some(entry) = self.current().entries.iter().find(|e| e.path == path).cloned() else {
            return;
        };
        if let Some(remote) = entry.remote {
            if network::is_listing(&remote.uri) {
                self.go(Location::Network(remote.uri), ctx);
            } else {
                self.apply(Action::Mount(remote.uri, entry.name), ctx);
            }
            return;
        }
        if entry.trashed.is_some() {
            self.message = Some(Message::warn(format!("Put “{}” back to open it.", entry.name)));
            return;
        }
        if self.picking.is_some() {
            match entry.kind {
                Kind::Folder | Kind::App => self.go(Location::Folder(entry.path), ctx),
                Kind::File => {
                    self.current_mut().select_only([entry.path.clone()]);
                    if let Some(picking) = &mut self.picking {
                        picking.name = entry.file_name();
                    }
                    self.accept(ctx);
                }
            }
            return;
        }
        match entry.kind {
            Kind::Folder => self.go(Location::Folder(entry.path), ctx),
            Kind::App => match patina_apps::read(&entry.path).and_then(|app| app.launch().map_err(|e| e.to_string())) {
                Ok(()) => {}
                Err(e) => self.message = Some(Message::error(format!("Can't open {}: {e}", entry.name))),
            },
            Kind::File if entry.script => {
                let dir = entry.path.parent().unwrap_or(Path::new("/"));
                if let Err(e) = spawn_in(&entry.path.to_string_lossy(), &[], dir) {
                    self.message = Some(Message::error(format!("Can't run {}: {e}", entry.name)));
                }
            }
            Kind::File => {
                if let Err(e) = patina::open::path(&entry.path) {
                    self.message = Some(Message::error(format!("Can't open {}: {e}", entry.name)));
                }
            }
        }
    }

    /// The accept button: answers with what's chosen, or goes into the one
    /// folder selected; saving over a file asks first.
    fn accept(&mut self, ctx: &egui::Context) {
        let Some(picking) = &self.picking else { return };
        let tab = self.current();
        let Some(folder) = tab.folder().map(Path::to_path_buf) else { return };
        let selection: Vec<Entry> = tab.selection().into_iter().cloned().collect();
        let folders: Vec<&Entry> = selection.iter().filter(|e| e.kind != Kind::File).collect();
        let paths = if picking.request.save {
            let name = picking.name.trim();
            if name.is_empty() {
                return;
            }
            let path = folder.join(name);
            if path.is_dir() {
                self.go(Location::Folder(path), ctx);
                return;
            }
            if path.exists() && !matches!(self.confirm, Some(Confirm::Replace(_))) {
                self.confirm = Some(Confirm::Replace(path));
                return;
            }
            vec![path]
        } else if picking.request.directory {
            if folders.is_empty() { vec![folder.clone()] } else { folders.iter().map(|e| e.path.clone()).collect() }
        } else {
            if let [only] = &selection[..]
                && only.kind != Kind::File
            {
                let path = only.path.clone();
                self.go(Location::Folder(path), ctx);
                return;
            }
            let files: Vec<PathBuf> =
                selection.iter().filter(|e| e.kind == Kind::File).map(|e| e.path.clone()).collect();
            if files.is_empty() {
                return;
            }
            if picking.request.multiple { files } else { files.into_iter().take(1).collect() }
        };
        let answer = Answer { paths, filter: picking.filter };
        *picking.answer.lock().unwrap_or_else(|e| e.into_inner()) = Some(answer);
        self.config.chooser_folder = Some(folder);
        self.config.save();
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// A typed path: saved to as it is, or opened if it's a file.
    fn choose_path(&mut self, path: PathBuf, ctx: &egui::Context) {
        let Some(picking) = &mut self.picking else { return };
        let Some(parent) = path.parent().map(Path::to_path_buf) else { return };
        if picking.request.save {
            picking.name = places::name(&path);
            self.go(Location::Folder(parent), ctx);
            // Listing the folder isn't needed to save into it.
            self.accept(ctx);
        } else if path.is_file() {
            let answer = Answer { paths: vec![path], filter: picking.filter };
            *picking.answer.lock().unwrap_or_else(|e| e.into_inner()) = Some(answer);
            self.config.chooser_folder = Some(parent);
            self.config.save();
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn click(&mut self, path: PathBuf, add: bool, range: bool) {
        if let Some(picking) = &mut self.picking
            && picking.request.save
            && let Some(entry) = self.tabs[self.tab].entries.iter().find(|e| e.path == path && e.kind == Kind::File)
        {
            picking.name = entry.file_name();
        }
        let show_hidden = self.config.show_hidden;
        let tab = self.current_mut();
        if range && let Some(anchor) = tab.anchor.clone() {
            let visible = tab.visible(show_hidden);
            let at = |p: &Path| visible.iter().position(|&i| tab.entries[i].path == p);
            if let (Some(a), Some(b)) = (at(&anchor), at(&path)) {
                let (from, to) = (a.min(b), a.max(b));
                let span: Vec<PathBuf> = visible[from..=to].iter().map(|&i| tab.entries[i].path.clone()).collect();
                if !add {
                    tab.selected.clear();
                }
                tab.selected.extend(span);
                tab.cursor = Some(path);
                return;
            }
        }
        if add {
            if !tab.selected.remove(&path) {
                tab.selected.insert(path.clone());
            }
        } else {
            tab.selected = HashSet::from([path.clone()]);
        }
        tab.anchor = Some(path.clone());
        tab.cursor = Some(path);
    }

    /// Moves the cursor `step` items (a row is `columns`), selecting it, or
    /// extending the selection with shift.
    fn step(&mut self, step: isize, extend: bool) {
        let show_hidden = self.config.show_hidden;
        let tab = self.current_mut();
        let visible = tab.visible(show_hidden);
        if visible.is_empty() {
            return;
        }
        let at = tab.cursor.as_ref().and_then(|c| visible.iter().position(|&i| &tab.entries[i].path == c));
        let next = match at {
            None => {
                if step < 0 {
                    visible.len() - 1
                } else {
                    0
                }
            }
            Some(at) => (at as isize + step).clamp(0, visible.len() as isize - 1) as usize,
        };
        let path = tab.entries[visible[next]].path.clone();
        tab.scroll_to = Some(path.clone());
        if extend {
            self.click(path, false, true);
        } else {
            self.click(path, false, false);
        }
    }

    /// Before the selection goes away: select the item after it, or before.
    fn select_neighbour(&mut self) {
        let show_hidden = self.config.show_hidden;
        let tab = self.current_mut();
        let visible = tab.visible(show_hidden);
        let gone = |i: &&usize| tab.selected.contains(&tab.entries[**i].path);
        let last = visible.iter().rposition(|i| gone(&i));
        let first = visible.iter().position(|i| gone(&i));
        let next = last
            .and_then(|l| visible[l + 1..].first())
            .or_else(|| first.and_then(|f| visible[..f].last()))
            .map(|&i| tab.entries[i].path.clone());
        tab.select_after = next.into_iter().collect();
    }

    fn paste(&mut self, text: Option<String>) {
        let (paths, cut) = match (&self.clip, text) {
            (Some(clip), None) => (clip.paths.clone(), clip.cut),
            (Some(clip), Some(text)) if text == clip.text => (clip.paths.clone(), clip.cut),
            (_, Some(text)) => uri::clipboard_files(&text),
            (None, None) => return,
        };
        if paths.is_empty() {
            return;
        }
        let into = self.current().location.clone();
        match into {
            Location::Network(_) => {}
            Location::Trash => self.worker.start(Op::Trash(paths)),
            Location::Folder(into) if cut => {
                self.worker.start(Op::Move { sources: paths, into });
                self.clip = None;
            }
            Location::Folder(into) => self.worker.start(Op::Copy { sources: paths, into }),
        }
    }

    /// A drop of `paths` on a folder or the Trash: moves them, or copies
    /// with Ctrl or when it's onto another disk.
    fn drop_into(&mut self, paths: Vec<PathBuf>, into: Location, copy: bool) {
        match into {
            Location::Network(_) => {}
            Location::Trash => {
                if !self.current().in_trash() {
                    self.worker.start(Op::Trash(paths));
                }
            }
            Location::Folder(into) => {
                let paths: Vec<PathBuf> = paths.into_iter().filter(|p| *p != into).collect();
                let Some(first) = paths.first() else { return };
                let across = !ops::same_disk(first, &into);
                if copy || across {
                    self.worker.start(Op::Copy { sources: paths, into });
                } else if paths.iter().any(|p| p.parent() != Some(&into)) {
                    self.worker.start(Op::Move { sources: paths, into });
                }
            }
        }
    }

    fn start_rename(&mut self, path: &Path) {
        if self.current().folder().is_none() {
            return;
        }
        let Some(entry) = self.current().entries.iter().find(|e| e.path == path) else { return };
        self.renaming = Some(Renaming { path: path.to_path_buf(), text: entry.name.clone(), started: false });
    }

    fn finish_rename(&mut self, ctx: &egui::Context) {
        let Some(renaming) = self.renaming.take() else { return };
        let Some(entry) = self.current().entries.iter().find(|e| e.path == renaming.path) else { return };
        let mut name = renaming.text.trim().to_string();
        if entry.kind == Kind::App && !name.ends_with(".app") {
            name.push_str(".app");
        }
        if name == entry.file_name() {
            return;
        }
        match ops::rename(&renaming.path, &name) {
            Ok(to) => {
                let tab = self.current_mut();
                tab.select_after = vec![to];
                let location = tab.location.clone();
                self.load(location, ctx);
            }
            Err(e) => self.message = Some(Message::error(e)),
        }
    }

    fn tab_action(&mut self, action: TabAction, ctx: &egui::Context) {
        match action {
            TabAction::New => {
                let location = self.current().location.clone();
                self.tabs.insert(self.tab + 1, Tab::new(location.clone()));
                self.tab += 1;
                self.load(location, ctx);
            }
            TabAction::Show(i) => self.tab = i.min(self.tabs.len() - 1),
            TabAction::Close(i) if self.tabs.len() > 1 => {
                self.tabs.remove(i);
                if i < self.tab || self.tab == self.tabs.len() {
                    self.tab -= 1;
                }
            }
            TabAction::Close(_) => {}
        }
        self.renaming = None;
        self.location_edit = None;
    }

    // ---- Keys ----

    /// The shortcuts, taken before the widgets see them; none while a text
    /// field has the keys.
    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.text_edit_focused()
            || self.renaming.is_some()
            || self.confirm.is_some()
            || !self.asking.is_empty()
            || self.worker.status.lock().unwrap_or_else(|e| e.into_inner()).conflict.is_some()
        {
            return;
        }
        let mut actions = Vec::new();
        let mut typed = String::new();
        let columns = if self.config.view == View::Icons { self.columns as isize } else { 1 };
        let in_columns = self.in_columns();
        let page = if self.config.view == View::Icons { columns * 4 } else { 12 };
        let in_trash = self.current().in_trash();
        let picking = self.picking.is_some();
        let looking = self.looking;
        let properties = self.properties.is_some();
        // A space goes on with a name being typed, else it's Quick Look;
        // then the text the key also makes isn't typed.
        let typing = !self.typed.0.is_empty() && self.typed.1.elapsed() < TYPE_AHEAD;
        let mut looked = false;
        ctx.input_mut(|i| {
            i.events.retain(|event| {
                let action = match event {
                    Event::Copy => Action::Copy,
                    Event::Cut => Action::Cut,
                    Event::Paste(text) => Action::Paste(Some(text.clone())),
                    Event::Text(text) if looked && text == " " => {
                        looked = false;
                        return false;
                    }
                    Event::Text(text) if !text.chars().any(char::is_control) => {
                        typed.push_str(text);
                        return false;
                    }
                    Event::Key { key, pressed: true, modifiers: m, .. } => {
                        let plain = !m.command && !m.alt;
                        match key {
                            Key::Space if plain && !typing => {
                                looked = true;
                                Action::QuickLook
                            }
                            Key::Escape if looking => Action::QuickLook,
                            Key::Escape if properties => Action::CloseProperties,
                            Key::Enter if plain && picking => Action::Accept,
                            Key::Enter if m.alt && !m.command && !picking => Action::Properties,
                            Key::I if m.command && !picking => Action::Properties,
                            Key::Escape if picking => Action::Cancel,
                            Key::N | Key::T | Key::W | Key::Q if picking && m.command && !m.shift => return true,
                            Key::Enter if plain => match self.tabs[self.tab].selection().first() {
                                Some(e) if !m.shift => Action::Open(e.path.clone()),
                                _ => return true,
                            },
                            Key::ArrowLeft if m.alt => Action::Back,
                            Key::ArrowRight if m.alt => Action::Forward,
                            Key::ArrowUp if m.alt => Action::Up,
                            Key::ArrowDown if m.alt => match self.tabs[self.tab].selection().first() {
                                Some(e) => Action::Open(e.path.clone()),
                                None => return true,
                            },
                            Key::Home if m.alt => Action::Go(Location::Folder(dirs::home_dir().unwrap_or_default())),
                            Key::Backspace if plain => Action::Up,
                            Key::ArrowLeft if plain && in_columns => Action::Up,
                            Key::ArrowRight if plain && in_columns => Action::IntoColumn,
                            Key::ArrowLeft if plain && columns > 1 => Action::Move(-1),
                            Key::ArrowRight if plain && columns > 1 => Action::Move(1),
                            Key::ArrowUp if plain => Action::Move(-columns),
                            Key::ArrowDown if plain => Action::Move(columns),
                            Key::PageUp if plain => Action::Move(-page),
                            Key::PageDown if plain => Action::Move(page),
                            Key::Home if plain => Action::Move(isize::MIN / 2),
                            Key::End if plain => Action::Move(isize::MAX / 2),
                            Key::Delete if !m.command => {
                                if in_trash || m.shift {
                                    Action::Purge
                                } else {
                                    Action::Trash
                                }
                            }
                            Key::F2 if plain => Action::StartRename,
                            Key::Escape => Action::ClearSelection,
                            Key::A if m.command => Action::SelectAll,
                            Key::N if m.command && m.shift => Action::NewFolder,
                            Key::N if m.command => Action::NewWindow,
                            Key::Q if m.command => Action::Quit,
                            Key::H if m.command => Action::ToggleHidden,
                            Key::L if m.command => Action::EditLocation,
                            Key::F if m.command => Action::FocusFilter,
                            Key::Num1 if m.command => Action::SetView(View::Icons),
                            Key::Num2 if m.command => Action::SetView(View::List),
                            Key::Num3 if m.command => Action::SetView(View::Columns),
                            _ => return true,
                        }
                    }
                    _ => return true,
                };
                actions.push(action);
                false
            });
        });
        if !typed.is_empty() {
            self.type_ahead(&typed);
        }
        self.actions.extend(actions);
    }

    /// Selects the first item whose name starts with what's been typed.
    fn type_ahead(&mut self, typed: &str) {
        if self.typed.1.elapsed() > TYPE_AHEAD {
            self.typed.0.clear();
        }
        self.typed.0.push_str(&typed.to_lowercase());
        self.typed.1 = Instant::now();
        let show_hidden = self.config.show_hidden;
        let typed = self.typed.0.clone();
        let tab = self.current_mut();
        let found = tab
            .visible(show_hidden)
            .into_iter()
            .map(|i| &tab.entries[i])
            .find(|e| e.name.to_lowercase().starts_with(&typed))
            .map(|e| e.path.clone());
        if let Some(path) = found {
            tab.select_only([path]);
        }
    }

    // ---- Drawing ----

    fn header(&mut self, ui: &mut Ui, style: &FrameStyle, span: egui::Rangef) {
        if let Some(picking) = &self.picking {
            let title = picking.request.title();
            TitleBar::new(&title, style).custom(!self.config.system_title_bar).show(ui, |_| {}, |_| {});
            return;
        }
        let titles: Vec<String> = self.tabs.iter().map(|t| Self::title_of(&t.location, &self.mounts)).collect();
        let mut action = None;
        TitleBar::new("", style).custom(!self.config.system_title_bar).menu_bar(true).span(span).show(
            ui,
            |ui| {
                ui.menu_button("File", |ui| self.file_menu(ui));
                ui.menu_button("Edit", |ui| self.edit_menu(ui));
                ui.menu_button("View", |ui| self.view_menu(ui));
                ui.menu_button("Go", |ui| self.go_menu(ui));
                ui.menu_button("Skin", |ui| self.skin_menu(ui));
                action = TabStrip::new(&titles, self.tab, style).span(span).show(ui);
            },
            |_| {},
        );
        if let Some(action) = action {
            self.actions.push(Action::Tab(action));
        }
    }

    fn file_menu(&mut self, ui: &mut Ui) {
        let tab = self.current();
        let selection = tab.selection();
        let folder = tab.folder().map(Path::to_path_buf);
        let mut a = Vec::new();
        if ui.add(egui::Button::new("New Window").shortcut_text(shortcut(ui, "Ctrl+N"))).clicked() {
            a.push(Action::NewWindow);
        }
        if ui.add(egui::Button::new("New Tab").shortcut_text(shortcut(ui, "Ctrl+T"))).clicked() {
            a.push(Action::Tab(TabAction::New));
        }
        let new_folder = egui::Button::new("New Folder").shortcut_text(shortcut(ui, "Ctrl+Shift+N"));
        if ui.add_enabled(folder.is_some(), new_folder).clicked() {
            a.push(Action::NewFolder);
        }
        ui.separator();
        let one = selection.first().filter(|_| selection.len() == 1).copied();
        if ui.add_enabled(one.is_some(), egui::Button::new("Open").shortcut_text(shortcut(ui, "Enter"))).clicked()
            && let Some(e) = one
        {
            a.push(Action::Open(e.path.clone()));
        }
        let quick_look = egui::Button::selectable(self.looking, "Quick Look").shortcut_text(shortcut(ui, "Space"));
        if ui.add_enabled(!selection.is_empty(), quick_look).clicked() {
            a.push(Action::QuickLook);
        }
        if let Some(e) = one.filter(|e| e.kind == Kind::App)
            && ui.button("Show Contents").clicked()
        {
            a.push(Action::ShowContents(e.path.clone()));
        }
        if let Some(e) = one.filter(|e| e.is_folder())
            && ui.button("Open in New Tab").clicked()
        {
            a.push(Action::OpenInNewTab(Location::Folder(e.path.clone())));
        }
        if let Some(folder) = &folder
            && ui.button("Open Terminal Here").clicked()
        {
            a.push(Action::Terminal(folder.clone()));
        }
        ui.separator();
        let properties = egui::Button::new("Properties").shortcut_text(shortcut(ui, "Ctrl+I"));
        if ui.add_enabled(folder.is_some(), properties).clicked() {
            a.push(Action::Properties);
        }
        ui.separator();
        if ui.add(egui::Button::new("Close Tab").shortcut_text(shortcut(ui, "Ctrl+W"))).clicked() {
            a.push(Action::CloseTab);
        }
        self.actions.extend(a);
    }

    fn edit_menu(&mut self, ui: &mut Ui) {
        let tab = self.current();
        let in_trash = tab.in_trash();
        // Servers and shares can't be cut, copied, renamed or trashed.
        let count = if tab.remote() { 0 } else { tab.selected.len() };
        let can_paste = self.clip.is_some() && !tab.remote();
        let mut a = Vec::new();
        if in_trash {
            if ui.add_enabled(count > 0, egui::Button::new("Put Back")).clicked() {
                a.push(Action::Restore);
            }
            if ui.add_enabled(count > 0, egui::Button::new("Delete Permanently…")).clicked() {
                a.push(Action::Purge);
            }
            ui.separator();
            if ui.add_enabled(!tab.entries.is_empty(), egui::Button::new("Empty Trash…")).clicked() {
                a.push(Action::EmptyTrash);
            }
        } else {
            if ui.add_enabled(count > 0, egui::Button::new("Cut").shortcut_text(shortcut(ui, "Ctrl+X"))).clicked() {
                a.push(Action::Cut);
            }
            if ui.add_enabled(count > 0, egui::Button::new("Copy").shortcut_text(shortcut(ui, "Ctrl+C"))).clicked() {
                a.push(Action::Copy);
            }
            let paste = egui::Button::new("Paste").shortcut_text(shortcut(ui, "Ctrl+V"));
            if ui.add_enabled(can_paste, paste).clicked() {
                a.push(Action::Paste(None));
            }
            if ui.add_enabled(count > 0, egui::Button::new("Copy Path")).clicked() {
                a.push(Action::CopyPath);
            }
            ui.separator();
            if ui.add_enabled(count == 1, egui::Button::new("Rename…").shortcut_text(shortcut(ui, "F2"))).clicked() {
                a.push(Action::StartRename);
            }
            let trash = egui::Button::new("Move to Trash").shortcut_text(shortcut(ui, "Delete"));
            if ui.add_enabled(count > 0, trash).clicked() {
                a.push(Action::Trash);
            }
        }
        ui.separator();
        if ui.add(egui::Button::new("Select All").shortcut_text(shortcut(ui, "Ctrl+A"))).clicked() {
            a.push(Action::SelectAll);
        }
        self.actions.extend(a);
    }

    fn view_menu(&mut self, ui: &mut Ui) {
        let mut a = Vec::new();
        let view = self.config.view;
        if ui
            .add(egui::Button::selectable(view == View::Icons, "As Icons").shortcut_text(shortcut(ui, "Ctrl+1")))
            .clicked()
        {
            a.push(Action::SetView(View::Icons));
        }
        if ui
            .add(egui::Button::selectable(view == View::List, "As List").shortcut_text(shortcut(ui, "Ctrl+2")))
            .clicked()
        {
            a.push(Action::SetView(View::List));
        }
        if ui
            .add(egui::Button::selectable(view == View::Columns, "As Columns").shortcut_text(shortcut(ui, "Ctrl+3")))
            .clicked()
        {
            a.push(Action::SetView(View::Columns));
        }
        ui.separator();
        let mut hidden = self.config.show_hidden;
        if ui.checkbox(&mut hidden, "Show Hidden Files").clicked() {
            a.push(Action::ToggleHidden);
        }
        ui.separator();
        self.sort_menu(ui);
        ui.separator();
        let mut patina_frame = !self.config.system_title_bar;
        if ui.checkbox(&mut patina_frame, "Patina Frame").changed() {
            self.config.system_title_bar = !patina_frame;
            self.config.save();
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Decorations(!patina_frame));
        }
        self.actions.extend(a);
    }

    fn sort_menu(&mut self, ui: &mut Ui) {
        let mut sort = self.config.sort;
        for (by, label) in [
            (SortBy::Name, "Sort by Name"),
            (SortBy::Size, "Sort by Size"),
            (SortBy::Modified, "Sort by Date Modified"),
            (SortBy::Created, "Sort by Date Created"),
            (SortBy::Accessed, "Sort by Date Last Opened"),
            (SortBy::Kind, "Sort by Kind"),
        ] {
            ui.radio_value(&mut sort.by, by, label);
        }
        ui.checkbox(&mut sort.descending, "Reversed");
        if sort != self.config.sort {
            self.set_sort(sort);
        }
    }

    fn set_sort(&mut self, sort: listing::Sort) {
        self.config.sort = sort;
        self.config.save();
        for tab in &mut self.tabs {
            listing::sort(&mut tab.entries, sort);
        }
        for entries in self.column_lists.values_mut().filter_map(|l| l.as_mut().ok()) {
            listing::sort(entries, sort);
        }
    }

    fn go_menu(&mut self, ui: &mut Ui) {
        let tab = self.current();
        let mut a = Vec::new();
        if ui
            .add_enabled(!tab.back.is_empty(), egui::Button::new("Back").shortcut_text(shortcut(ui, "Alt+Left")))
            .clicked()
        {
            a.push(Action::Back);
        }
        if ui
            .add_enabled(!tab.forward.is_empty(), egui::Button::new("Forward").shortcut_text(shortcut(ui, "Alt+Right")))
            .clicked()
        {
            a.push(Action::Forward);
        }
        let up = self.up_from(&tab.location).is_some();
        if ui.add_enabled(up, egui::Button::new("Enclosing Folder").shortcut_text(shortcut(ui, "Alt+Up"))).clicked() {
            a.push(Action::Up);
        }
        ui.separator();
        for place in self.places.iter().chain(&self.disks) {
            if ui.button(&place.name).clicked() {
                a.push(Action::Go(place.location.clone()));
            }
        }
        if ui.button("Network").clicked() {
            a.push(Action::Go(Location::Network(network::ROOT.into())));
        }
        ui.separator();
        if ui.button("Connect to Server…").clicked() {
            a.push(Action::ConnectToServer);
        }
        if ui.add(egui::Button::new("Location…").shortcut_text(shortcut(ui, "Ctrl+L"))).clicked() {
            a.push(Action::EditLocation);
        }
        self.actions.extend(a);
    }

    /// Patina's skin menu (its ADR-0017): only controls, in the order every Patina app has them;
    /// what they did goes to the status bar.
    fn skin_menu(&mut self, ui: &mut Ui) {
        // Size the menu to its widest entry instead of wrapping entries to its first width.
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
        let ctx = ui.ctx().clone();
        let mut changed = false;
        for (i, skin) in self.skins.iter().enumerate() {
            let label = match &skin.author {
                Some(author) if skin.path.is_some() => format!("{} — {author}", skin.name),
                _ => skin.name.clone(),
            };
            let resp = ui.radio(self.skin == i, label);
            let resp = match &skin.path {
                Some(path) => resp.on_hover_text(path.display().to_string()),
                None => resp,
            };
            if resp.clicked() {
                self.skin = i;
                changed = true;
            }
        }
        ui.separator();
        ui.add_enabled_ui(self.skins[self.skin].follows_desktop(), |ui| {
            for (value, label) in [("system", "Follow desktop"), ("light", "Light"), ("dark", "Dark")] {
                if ui.radio(self.config.light_dark == value, label).clicked() {
                    self.config.light_dark = value.into();
                    changed = true;
                }
            }
        });
        ui.separator();
        if ui.button("Reload skins").clicked() {
            self.reload_skins();
            changed = true;
        }
        if let Some(dir) = skin::app_dir(APP) {
            if ui
                .button("Copy built-in skins to folder")
                .on_hover_text("Puts editable copies in the skins folder; never overwrites existing files")
                .clicked()
            {
                self.message = Some(match skin::export_built_ins(&dir, skin::BUILT_IN) {
                    Ok(export) => {
                        // The copies share the built-ins' names, so after a reload they're the
                        // versions in use.
                        self.reload_skins();
                        changed = true;
                        let mut parts = Vec::new();
                        if !export.copied.is_empty() {
                            parts.push(format!("Copied {}", export.copied.join(", ")));
                        }
                        if !export.skipped.is_empty() {
                            parts.push(format!("kept your existing {}", export.skipped.join(", ")));
                        }
                        Message::info(parts.join("; "))
                    }
                    Err(e) => Message::error(format!("Couldn't copy skins: {e}")),
                });
            }
            if ui.button("Open skins folder").on_hover_text(dir.display().to_string()).clicked() {
                let _ = std::fs::create_dir_all(&dir);
                self.actions.push(Action::OpenInNewTab(Location::Folder(dir)));
            }
        }
        if changed {
            self.config.skin = self.skins[self.skin].name.clone();
            self.config.save();
            self.apply_skin(&ctx);
        }
    }

    /// Loads the skins again, keeping the one in use by name.
    fn reload_skins(&mut self) {
        let name = self.skins[self.skin].name.clone();
        (self.skins, self.skin_errors) = load_skins();
        self.skin = self.skins.iter().position(|s| s.name == name).unwrap_or(0);
    }

    /// Back, forward and up; the path, or a field to type one; the view's
    /// buttons and the filter.
    fn toolbar(&mut self, ui: &mut Ui) {
        let tab = self.current();
        let (can_back, can_forward) = (!tab.back.is_empty(), !tab.forward.is_empty());
        let can_up = self.up_from(&tab.location).is_some();
        let in_trash = tab.in_trash();
        let has_entries = !tab.entries.is_empty();
        let mut a = Vec::new();
        ui.horizontal(|ui| {
            ui.set_min_height(TOOLBAR_ROW);
            ui.spacing_mut().item_spacing.x = 2.0;
            let back = glyph_button(ui, Glyph::Back, can_back, "Back (Alt+Left)");
            self.toolbar_middle = back.rect.center().y;
            if back.clicked() {
                a.push(Action::Back);
            }
            if glyph_button(ui, Glyph::Forward, can_forward, "Forward (Alt+Right)").clicked() {
                a.push(Action::Forward);
            }
            if glyph_button(ui, Glyph::Up, can_up, "Enclosing folder (Alt+Up)").clicked() {
                a.push(Action::Up);
            }
            ui.add_space(8.0);

            // From the right: the filter, the view, the Trash's button.
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                let filter = egui::TextEdit::singleline(&mut self.tabs[self.tab].filter)
                    .hint_text("Filter")
                    .desired_width(150.0);
                let resp = ui.add(filter);
                if std::mem::take(&mut self.focus_filter) {
                    resp.request_focus();
                }
                if resp.has_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.tabs[self.tab].filter.clear();
                }
                ui.add_space(6.0);
                let view = self.config.view;
                if glyph_toggle(ui, Glyph::Columns, view == View::Columns, "As columns (Ctrl+3)").clicked() {
                    a.push(Action::SetView(View::Columns));
                }
                if glyph_toggle(ui, Glyph::List, view == View::List, "As list (Ctrl+2)").clicked() {
                    a.push(Action::SetView(View::List));
                }
                if glyph_toggle(ui, Glyph::Icons, view == View::Icons, "As icons (Ctrl+1)").clicked() {
                    a.push(Action::SetView(View::Icons));
                }
                if in_trash {
                    ui.add_space(6.0);
                    if ui.add_enabled(has_entries, egui::Button::new("Empty Trash…")).clicked() {
                        a.push(Action::EmptyTrash);
                    }
                }
                ui.add_space(6.0);
                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| self.path_bar(ui, &mut a));
            });
        });
        self.actions.extend(a);
    }

    /// The folder's path as buttons, each a place to go or drop on; Ctrl+L
    /// or a click on the bar beside the crumbs turns it into a field.
    fn path_bar(&mut self, ui: &mut Ui, a: &mut Vec<Action>) {
        if let Some(text) = &mut self.location_edit {
            let all = text.chars().count();
            let resp = ui.add(egui::TextEdit::singleline(text).desired_width(ui.available_width()));
            if !self.location_started {
                // All of it selected, so typing replaces it, or the cursor at the end.
                self.location_started = true;
                let from = if self.location_select { 0 } else { all };
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(from), CCursor::new(all))));
                    state.store(ui.ctx(), resp.id);
                }
                resp.request_focus();
            }
            if resp.lost_focus() {
                let text = self.location_edit.take().unwrap_or_default();
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    let path = expand(&text);
                    let text = text.trim();
                    if text.contains("://") && !text.starts_with("file://") {
                        a.push(Action::OpenUri(text.to_string()));
                    } else if path.is_dir() {
                        a.push(Action::Go(Location::Folder(path)));
                    } else if self.picking.is_some() && (path.is_file() || path.parent().is_some_and(Path::is_dir)) {
                        a.push(Action::ChoosePath(path));
                    } else if let Some(parent) = path.parent().filter(|_| path.exists()) {
                        a.push(Action::Go(Location::Folder(parent.to_path_buf())));
                        self.tabs[self.tab].select_after = vec![path];
                    } else {
                        self.message = Some(Message::error(format!("There's no folder at {}", path.display())));
                    }
                }
            }
            return;
        }
        let crumbs = crumbs(&self.current().location, &self.mounts);
        let bar = Rect::from_center_size(
            pos2(ui.cursor().min.x + ui.available_width() / 2.0, self.toolbar_middle),
            vec2(ui.available_width(), TOOLBAR_ROW),
        );
        let background = ui.interact(bar, ui.id().with("path-bar"), Sense::click());
        // Crumbs and the drop targets take their own clicks; anywhere else on
        // the bar is the field.
        if background.clicked() {
            a.push(Action::EditLocation);
        }
        ui.scope_builder(egui::UiBuilder::new().max_rect(bar).layout(egui::Layout::top_down(Align::Min)), |ui| {
            egui::ScrollArea::horizontal()
                .content_margin(egui::Margin::ZERO)
                .stick_to_right(true)
                .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // As tall as the toolbar, so the crumbs sit in its middle.
                        ui.set_min_height(TOOLBAR_ROW);
                        ui.spacing_mut().item_spacing.x = 0.0;
                        let last = crumbs.len().saturating_sub(1);
                        for (i, (name, location)) in crumbs.into_iter().enumerate() {
                            if i > 0 {
                                let (rect, _) = ui.allocate_exact_size(vec2(14.0, 20.0), Sense::hover());
                                glyphs::paint(
                                    ui.painter(),
                                    rect,
                                    Glyph::Separator,
                                    ui.visuals().widgets.inactive.fg_stroke,
                                );
                            }
                            let text =
                                if i == last { egui::RichText::new(name).strong() } else { egui::RichText::new(name) };
                            let resp = ui.add(egui::Button::new(text).frame(false).min_size(vec2(0.0, TOOLBAR_ROW)));
                            if i != last && resp.clicked() {
                                a.push(Action::Go(location.clone()));
                            }
                            drop_target(ui, &resp, &location, a);
                        }
                    });
                });
        });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let here = self.current().location.clone();
        let px = (PLACE_ICON * ui.ctx().pixels_per_point()).round() as u32;
        let mut a = Vec::new();
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(6.0);
            let favourites: Vec<Place> = self.config.favourites.iter().map(|p| places::favourite(p)).collect();
            for (heading, list) in [("Places", &self.places), ("Favourites", &favourites), ("Disks", &self.disks)] {
                if list.is_empty() {
                    continue;
                }
                ui.label(egui::RichText::new(heading).small().weak());
                for place in list {
                    let icon = self.icons.get(&place.icon, px);
                    let resp = place_row(ui, &place.name, icon, place.location == here, 0.0);
                    if resp.clicked() {
                        a.push(Action::Go(place.location.clone()));
                    }
                    if resp.clicked_by(egui::PointerButton::Middle) {
                        a.push(Action::OpenInNewTab(place.location.clone()));
                    }
                    drop_target(ui, &resp, &place.location, &mut a);
                    if heading == "Favourites"
                        && let Location::Folder(path) = &place.location
                    {
                        resp.context_menu(|ui| {
                            if ui.button("Open in New Tab").clicked() {
                                a.push(Action::OpenInNewTab(place.location.clone()));
                            }
                            if ui.button("Remove from Favourites").clicked() {
                                a.push(Action::RemoveFavourite(path.clone()));
                            }
                        });
                    }
                }
                ui.add_space(10.0);
            }
            ui.label(egui::RichText::new("Network").small().weak());
            let root = Location::Network(network::ROOT.into());
            let icon = self.icons.get(&icons::theme(&["network-workgroup", "network-server", "folder-remote"]), px);
            let resp = place_row(ui, "Network", icon, here == root, 0.0);
            if resp.clicked() {
                a.push(Action::Go(root.clone()));
            }
            if resp.clicked_by(egui::PointerButton::Middle) {
                a.push(Action::OpenInNewTab(root));
            }
            let share_icon = icons::theme(&["folder-remote", "network-server", "folder"]);
            for mount in &self.mounts {
                let location = Location::Folder(mount.path.clone());
                let icon = self.icons.get(&share_icon, px);
                let resp = place_row(ui, &mount.name, icon, here == location, 28.0).on_hover_text(&mount.uri);
                // The eject button, on top of the row so it takes its own clicks.
                let eject = Rect::from_center_size(resp.rect.right_center() - vec2(14.0, 0.0), vec2(22.0, 22.0));
                let eject = ui.interact(eject, Id::new(("eject", &mount.uri)), Sense::click()).on_hover_text("Unmount");
                let stroke = if eject.hovered() {
                    ui.visuals().widgets.hovered.fg_stroke
                } else {
                    Stroke::new(1.0, ui.visuals().weak_text_color())
                };
                glyphs::paint(ui.painter(), eject.rect, Glyph::Eject, stroke);
                if eject.clicked() {
                    a.push(Action::Unmount(mount.uri.clone()));
                } else if resp.clicked() {
                    a.push(Action::Go(location.clone()));
                }
                if resp.clicked_by(egui::PointerButton::Middle) {
                    a.push(Action::OpenInNewTab(location.clone()));
                }
                patina::menu::context(&resp, |ui| {
                    if ui.button("Open in New Tab").clicked() {
                        a.push(Action::OpenInNewTab(location.clone()));
                    }
                    if ui.button("Unmount").clicked() {
                        a.push(Action::Unmount(mount.uri.clone()));
                    }
                });
                drop_target(ui, &resp, &location, &mut a);
            }
        });
        self.actions.extend(a);
    }

    /// While choosing: the name to save as, the file types, Cancel and the
    /// accept button.
    fn chooser_bar(&mut self, ui: &mut Ui) {
        let Some(picking) = &mut self.picking else { return };
        let mut a = Vec::new();
        let mut types_changed = false;
        ui.horizontal(|ui| {
            if picking.request.save {
                ui.label("Name:");
                let field = ui.add(egui::TextEdit::singleline(&mut picking.name).desired_width(260.0));
                if !picking.name_started {
                    picking.name_started = true;
                    // The name without its extension, ready to type over.
                    let stem = ops::split_extension(&picking.name).0.chars().count();
                    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), field.id) {
                        state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(stem))));
                        state.store(ui.ctx(), field.id);
                    }
                    field.request_focus();
                }
                if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    a.push(Action::Accept);
                }
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let label = picking.request.accept_label();
                if ui.add(egui::Button::new(egui::RichText::new(label).strong()).min_size(vec2(80.0, 0.0))).clicked() {
                    a.push(Action::Accept);
                }
                if ui.add(egui::Button::new("Cancel").min_size(vec2(80.0, 0.0))).clicked() {
                    a.push(Action::Cancel);
                }
                if let Some(message) = &self.message {
                    ui.add(egui::Label::new(message.text(ui)).truncate());
                }
                if !picking.request.filters.is_empty() && !picking.request.directory {
                    let filters = &picking.request.filters;
                    let shown = picking.filter.and_then(|i| filters.get(i)).map_or("All files", |f| f.name.as_str());
                    egui::ComboBox::from_id_salt("types").selected_text(shown).show_ui(ui, |ui| {
                        for (i, filter) in filters.iter().enumerate() {
                            types_changed |= ui.selectable_value(&mut picking.filter, Some(i), &filter.name).clicked();
                        }
                    });
                }
            });
        });
        if types_changed {
            let types = picking.types();
            for tab in &mut self.tabs {
                tab.types = types.clone();
            }
        }
        self.actions.extend(a);
    }

    /// Patina's status bar (its ADR-0016): the version, then the latest message, coloured by its
    /// level; on the right, what's running and the folder's count. Skins that failed to load stay
    /// there, ahead of newer messages, until they load.
    fn status_bar(&mut self, ui: &mut Ui) {
        let status = self.worker.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let tab = self.current();
        let visible = tab.visible(self.config.show_hidden).len();
        let selection = tab.selection();
        let items = |n: usize| if n == 1 { "1 item".to_string() } else { format!("{n} items") };
        let count = if selection.is_empty() {
            items(visible)
        } else {
            let bytes: u64 = selection.iter().filter(|e| e.kind == Kind::File).map(|e| e.size).sum();
            let size = if bytes > 0 { format!(", {}", listing::human_size(bytes)) } else { String::new() };
            format!("{} of {} selected{size}", selection.len(), visible)
        };
        let skin_errors = (!self.skin_errors.is_empty()).then(|| {
            let n = self.skin_errors.len();
            let mut message = Message::error(format!("Couldn't load {n} skin{}", if n == 1 { "" } else { "s" }));
            message.detail = self.skin_errors.clone();
            message
        });
        let message = skin_errors.as_ref().or(self.message.as_ref());
        ui.horizontal(|ui| {
            // Just the version: the app's name is already in the title bar and the window list.
            let version = egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION")));
            ui.label(version.color(ui.visuals().weak_text_color()).small());
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(count);
                if let Some((_, _, Some(info))) = &self.zfs {
                    ui.separator();
                    if zfs_label(ui, info, self.config.zfs_panel).clicked() {
                        self.config.zfs_panel = !self.config.zfs_panel;
                        self.config.save();
                    }
                }
                if let Some((_, name)) = &self.connecting {
                    ui.separator();
                    ui.spinner();
                    ui.label(format!("Connecting to {name}…"));
                }
                if let Some((label, progress)) = &status.running {
                    ui.separator();
                    if ui.small_button("Stop").clicked() {
                        self.worker.cancel();
                    }
                    if let Some(progress) = progress {
                        ui.add(egui::ProgressBar::new(*progress).desired_width(140.0));
                    } else {
                        ui.spinner();
                    }
                    let queued =
                        if status.queued > 0 { format!(" (and {} more)", status.queued) } else { String::new() };
                    ui.label(format!("{label}{queued}"));
                }
                ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                    if let Some(message) = message {
                        ui.separator();
                        let resp = ui.add(egui::Label::new(message.text(ui)).truncate());
                        if !message.detail.is_empty() {
                            resp.on_hover_text(message.detail.join("\n"));
                        }
                    }
                });
            });
        });
    }

    /// The folder's items, as icons or a list, with the background taking
    /// clicks, drags (a band to select with) and drops.
    fn items(&mut self, ui: &mut Ui) {
        let tab = self.current();
        if let Some(error) = &tab.error {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new(format!("Can't show this folder: {error}")).weak())
            });
            return;
        }
        let visible = tab.visible(self.config.show_hidden);
        let area = ui.available_rect_before_wrap();
        let background = ui.interact(area, ui.id().with("background"), Sense::click_and_drag());

        let in_columns = self.in_columns();
        if visible.is_empty() && (tab.loaded || tab.remote()) && !in_columns {
            let empty = if !tab.loaded && tab.location == Location::Network(network::ROOT.into()) {
                "Looking for servers…"
            } else if !tab.loaded {
                "Connecting…"
            } else if tab.location == Location::Network(network::ROOT.into()) {
                "No servers found"
            } else if tab.remote() {
                "No shared folders"
            } else if tab.in_trash() {
                "The Trash is empty"
            } else if tab.filter.is_empty() {
                "Empty folder"
            } else {
                "Nothing matches"
            };
            ui.painter().text(
                area.center(),
                Align2::CENTER_CENTER,
                empty,
                FontId::proportional(15.0),
                ui.visuals().weak_text_color(),
            );
        }

        match self.config.view {
            View::Icons => self.icon_view(ui, &visible),
            View::Columns if in_columns => self.column_view(ui, &visible),
            View::List | View::Columns => self.list_view(ui, &visible),
        }

        self.background(ui, &background, &visible);
    }

    fn background(&mut self, ui: &mut Ui, background: &Response, visible: &[usize]) {
        let ctx = ui.ctx().clone();
        let tab = self.current();
        let location = tab.location.clone();
        let folder = tab.folder().map(Path::to_path_buf);
        if background.clicked() && !ui.input(|i| i.modifiers.command) {
            self.actions.push(Action::ClearSelection);
        }
        if background.secondary_clicked() {
            self.actions.push(Action::ClearSelection);
        }
        let in_trash = self.current().in_trash();
        let can_paste = self.clip.is_some();
        let mut hidden = self.config.show_hidden;
        let mut a = Vec::new();
        let remote = self.current().remote();
        patina::menu::context(background, |ui| {
            if remote {
                if ui.button("Connect to Server…").clicked() {
                    a.push(Action::ConnectToServer);
                }
                return;
            }
            if in_trash {
                if ui.button("Empty Trash…").clicked() {
                    a.push(Action::EmptyTrash);
                }
                return;
            }
            if ui.button("New Folder").clicked() {
                a.push(Action::NewFolder);
            }
            if ui.add_enabled(can_paste, egui::Button::new("Paste")).clicked() {
                a.push(Action::Paste(None));
            }
            ui.separator();
            if let Some(folder) = &folder
                && ui.button("Open Terminal Here").clicked()
            {
                a.push(Action::Terminal(folder.clone()));
            }
            ui.separator();
            if ui.checkbox(&mut hidden, "Show Hidden Files").clicked() {
                a.push(Action::ToggleHidden);
            }
            if folder.is_some() {
                ui.separator();
                if ui.button("Properties").clicked() {
                    a.push(Action::Properties);
                }
            }
        });
        self.actions.extend(a);

        // A drop on the background lands in this folder.
        if let Some(dragged) = background.dnd_release_payload::<Dragged>() {
            let into_self = dragged.0.iter().all(|p| p.parent() == folder.as_deref());
            if !into_self || ctx.input(|i| i.modifiers.ctrl) {
                self.actions.push(Action::Drop { paths: dragged.0.clone(), into: location });
            }
        }

        // The band: everything it touches is selected.
        let pointer = ctx.pointer_interact_pos();
        if background.drag_started()
            && let Some(p) = pointer
        {
            let keep = if ui.input(|i| i.modifiers.command) { self.current().selected.clone() } else { HashSet::new() };
            self.band = Some((p - self.origin.to_vec2(), keep));
        }
        if let (Some((start, keep)), Some(p)) = (&self.band, pointer) {
            let content = Rect::from_two_pos(*start, p - self.origin.to_vec2());
            let mut selected = keep.clone();
            let tab = self.current();
            for (n, &i) in visible.iter().enumerate() {
                if self.item_rect(n, ui.available_width()).intersects(content) {
                    selected.insert(tab.entries[i].path.clone());
                }
            }
            self.current_mut().selected = selected;
            let screen = content.translate(self.origin.to_vec2());
            let v = ui.visuals();
            let painter = ui.ctx().layer_painter(LayerId::new(Order::Foreground, Id::new("band")));
            painter.rect(
                screen,
                2.0,
                v.selection.bg_fill.gamma_multiply(0.25),
                Stroke::new(1.0, v.selection.bg_fill),
                StrokeKind::Inside,
            );
        }
        if self.band.is_some() && !background.dragged() {
            self.band = None;
        }

        // Files dropped from another app are copied in.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if !dropped.is_empty() {
            self.actions.push(Action::Drop { paths: dropped, into: self.current().location.clone() });
        }
    }

    /// The `n`th visible item's place in the content, before scrolling.
    fn item_rect(&self, n: usize, width: f32) -> Rect {
        match self.config.view {
            View::Icons => {
                let columns = self.columns.max(1);
                let cell = vec2(width / columns as f32, CELL.y);
                let (row, col) = (n / columns, n % columns);
                Rect::from_min_size(pos2(col as f32 * cell.x, row as f32 * cell.y), cell).shrink(6.0)
            }
            View::Columns if self.in_columns() => Rect::from_min_size(pos2(0.0, n as f32 * ROW), vec2(COLUMN, ROW)),
            View::List | View::Columns => Rect::from_min_size(pos2(0.0, n as f32 * ROW), vec2(width, ROW)),
        }
    }

    fn icon_view(&mut self, ui: &mut Ui, visible: &[usize]) {
        let width = ui.available_width();
        let columns = ((width / CELL.x).floor() as usize).max(1);
        self.columns = columns;
        let cell_w = width / columns as f32;
        let rows = visible.len().div_ceil(columns);
        let px = (ICON * ui.ctx().pixels_per_point()).round() as u32;
        let mut area = egui::ScrollArea::vertical().auto_shrink(false).id_salt(("icons", self.tab));
        if let Some(n) = self.scroll_target(visible) {
            area = area.vertical_scroll_offset(self.reveal((n / columns) as f32 * CELL.y, CELL.y));
        }
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let output = area.show_rows(ui, CELL.y, rows, |ui, range| {
                self.origin = pos2(ui.max_rect().left(), ui.min_rect().top() - range.start as f32 * CELL.y);
                for row in range {
                    let (row_rect, _) = ui.allocate_exact_size(vec2(width, CELL.y), Sense::hover());
                    for col in 0..columns {
                        let Some(&i) = visible.get(row * columns + col) else { break };
                        let cell =
                            Rect::from_min_size(row_rect.min + vec2(col as f32 * cell_w, 0.0), vec2(cell_w, CELL.y));
                        self.icon_item(ui, i, cell, px);
                    }
                }
            });
            self.viewport = (output.state.offset.y, output.inner_rect.height());
        });
    }

    fn icon_item(&mut self, ui: &mut Ui, i: usize, cell: Rect, px: u32) {
        let entry = self.current().entries[i].clone();
        let source = self.icon_source(&entry);
        let texture = self.icons.get(&source, px);
        let selected = self.current().selected.contains(&entry.path);
        let inner = cell.shrink(6.0);
        let resp = ui.interact(inner, Id::new(("item", &entry.path)), Sense::click_and_drag());
        let v = ui.visuals().clone();
        let painter = ui.painter_at(cell);
        let icon = Rect::from_center_size(pos2(inner.center().x, inner.top() + 4.0 + ICON / 2.0), vec2(ICON, ICON));
        let dropping =
            entry.is_folder() && resp.dnd_hover_payload::<Dragged>().is_some_and(|d| !d.0.contains(&entry.path));
        if selected || dropping {
            painter.rect_filled(icon.expand(4.0), CornerRadius::same(6), v.widgets.hovered.weak_bg_fill);
        } else if resp.hovered() {
            painter.rect_filled(
                icon.expand(4.0),
                CornerRadius::same(6),
                v.widgets.hovered.weak_bg_fill.gamma_multiply(0.6),
            );
        }
        let dim = self.clip.as_ref().is_some_and(|c| c.cut && c.paths.contains(&entry.path));
        let tint = if dim { egui::Color32::WHITE.gamma_multiply(0.45) } else { egui::Color32::WHITE };
        if let Some(texture) = texture {
            painter.image(texture, icon, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
        }
        if entry.symlink {
            link_badge(&painter, icon, &v);
        }

        let label_top = icon.bottom() + 6.0;
        if self.renaming.as_ref().is_some_and(|r| r.path == entry.path) {
            let field = Rect::from_min_size(pos2(cell.left() + 2.0, label_top - 2.0), vec2(cell.width() - 4.0, 22.0));
            self.rename_field(ui, field, true);
        } else {
            let colour = if selected { v.selection.stroke.color } else { v.text_color() };
            let mut job = LayoutJob::single_section(
                entry.name.clone(),
                TextFormat { font_id: egui::TextStyle::Body.resolve(ui.style()), color: colour, ..Default::default() },
            );
            job.wrap.max_width = inner.width() - 4.0;
            job.wrap.max_rows = if selected { 4 } else { 2 };
            job.wrap.break_anywhere = true;
            job.halign = Align::Center;
            let galley = ui.painter().layout_job(job);
            let at = pos2(inner.center().x, label_top);
            if selected {
                let back = galley.rect.translate(at.to_vec2()).expand2(vec2(4.0, 1.0));
                ui.painter().rect_filled(back, CornerRadius::same(4), v.selection.bg_fill);
            }
            let painter = if selected { ui.painter().clone() } else { painter };
            painter.galley(at, galley, colour);
        }
        self.item_input(ui, &resp, &entry, selected, None);
    }

    fn list_view(&mut self, ui: &mut Ui, visible: &[usize]) {
        let width = ui.available_width();
        let columns = list_columns(width, &self.config.list_columns);
        self.columns = 1;
        // The header: click a column to sort by it, again to reverse.
        let (header, _) = ui.allocate_exact_size(vec2(width, ROW), Sense::hover());
        // Made first so the columns, made after, sit on top and get the clicks.
        let header_resp = ui.interact(header, Id::new(("list header", self.tab)), Sense::click());
        let mut x = header.left();
        for (by, label, w, right) in &columns {
            let rect = Rect::from_min_size(pos2(x, header.top()), vec2(*w, ROW));
            let resp = ui.interact(rect, Id::new(("column", label)), Sense::click());
            let sorted = self.config.sort.by == *by;
            let gap = if sorted { 14.0 } else { 0.0 };
            let (align, at) = if *right {
                (Align2::RIGHT_CENTER, rect.right_center() - vec2(8.0 + gap, 0.0))
            } else {
                (Align2::LEFT_CENTER, rect.left_center() + vec2(8.0, 0.0))
            };
            let colour = if resp.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
            let text_rect = ui.painter().text(at, align, *label, egui::TextStyle::Small.resolve(ui.style()), colour);
            if sorted {
                // Painted, not a glyph: the fonts have no triangles.
                let c = pos2(text_rect.right() + 8.0, rect.center().y);
                let (base, tip) = if self.config.sort.descending { (-2.0, 2.0) } else { (2.0, -2.0) };
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![pos2(c.x - 3.5, c.y + base), pos2(c.x + 3.5, c.y + base), pos2(c.x, c.y + tip)],
                    colour,
                    egui::Stroke::NONE,
                ));
            }
            if resp.clicked() {
                let mut sort = self.config.sort;
                if sort.by == *by {
                    sort.descending = !sort.descending;
                } else {
                    sort = listing::Sort { by: *by, descending: false };
                }
                self.set_sort(sort);
            }
            x += w;
        }
        header_resp.context_menu(|ui| {
            let mut chosen = self.config.list_columns.clone();
            for (by, label) in OPTIONAL_COLUMNS {
                let mut on = chosen.contains(&by);
                if ui.checkbox(&mut on, label).changed() {
                    if on {
                        chosen.push(by);
                    } else {
                        chosen.retain(|c| *c != by);
                    }
                }
            }
            if chosen != self.config.list_columns {
                self.config.list_columns = chosen;
                self.config.save();
            }
        });
        ui.painter().hline(header.x_range(), header.bottom(), ui.visuals().widgets.noninteractive.bg_stroke);

        let px = (ROW_ICON * ui.ctx().pixels_per_point()).round() as u32;
        let mut area = egui::ScrollArea::vertical().auto_shrink(false).id_salt(("list", self.tab));
        if let Some(n) = self.scroll_target(visible) {
            area = area.vertical_scroll_offset(self.reveal(n as f32 * ROW, ROW));
        }
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let output = area.show_rows(ui, ROW, visible.len(), |ui, range| {
                self.origin = pos2(ui.max_rect().left(), ui.min_rect().top() - range.start as f32 * ROW);
                for n in range {
                    let (rect, _) = ui.allocate_exact_size(vec2(width, ROW), Sense::hover());
                    self.list_row(ui, visible[n], n, rect, &columns, px);
                }
            });
            self.viewport = (output.state.offset.y, output.inner_rect.height());
        });
    }

    fn list_row(
        &mut self,
        ui: &mut Ui,
        i: usize,
        n: usize,
        rect: Rect,
        columns: &[(SortBy, &str, f32, bool)],
        px: u32,
    ) {
        let entry = self.current().entries[i].clone();
        let selected = self.current().selected.contains(&entry.path);
        let resp = ui.interact(rect, Id::new(("item", &entry.path)), Sense::click_and_drag());
        let v = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        let dropping =
            entry.is_folder() && resp.dnd_hover_payload::<Dragged>().is_some_and(|d| !d.0.contains(&entry.path));
        if selected {
            painter.rect_filled(rect, CornerRadius::same(4), v.selection.bg_fill);
        } else if dropping || resp.hovered() {
            painter.rect_filled(rect, CornerRadius::same(4), v.widgets.hovered.weak_bg_fill);
        } else if n % 2 == 1 {
            painter.rect_filled(rect, CornerRadius::ZERO, v.faint_bg_color);
        }
        let colour = if selected { v.selection.stroke.color } else { v.text_color() };
        let weak = if selected { colour } else { v.weak_text_color() };
        let font = egui::TextStyle::Body.resolve(ui.style());

        let name_w = columns[0].2;
        let icon =
            Rect::from_center_size(pos2(rect.left() + 8.0 + ROW_ICON / 2.0, rect.center().y), vec2(ROW_ICON, ROW_ICON));
        if let Some(texture) = self.icons.get(&self.icon_source(&entry), px) {
            let dim = self.clip.as_ref().is_some_and(|c| c.cut && c.paths.contains(&entry.path));
            let tint = if dim { egui::Color32::WHITE.gamma_multiply(0.45) } else { egui::Color32::WHITE };
            painter.image(texture, icon, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
        }
        if entry.symlink {
            link_badge(&painter, icon, &v);
        }
        let text_left = icon.right() + 8.0;
        if self.renaming.as_ref().is_some_and(|r| r.path == entry.path) {
            let field = Rect::from_min_max(
                pos2(text_left - 4.0, rect.top() + 1.0),
                pos2(rect.left() + name_w - 8.0, rect.bottom() - 1.0),
            );
            self.rename_field(ui, field, false);
        } else {
            let mut job = LayoutJob::single_section(
                entry.name.clone(),
                TextFormat { font_id: font.clone(), color: colour, ..Default::default() },
            );
            job.wrap.max_width = name_w - (text_left - rect.left()) - 8.0;
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.painter().layout_job(job);
            painter.galley(pos2(text_left, rect.center().y - galley.size().y / 2.0), galley, colour);
        }

        let mut x = rect.left() + name_w;
        for (by, _, w, right) in &columns[1..] {
            let text = match by {
                SortBy::Size => match entry.kind {
                    Kind::Folder if entry.trashed.is_none() && entry.remote.is_none() => {
                        if entry.size == 1 {
                            "1 item".into()
                        } else {
                            format!("{} items", entry.size)
                        }
                    }
                    Kind::File => listing::human_size(entry.size),
                    _ => "—".into(),
                },
                SortBy::Modified => entry.modified.map(when).unwrap_or_default(),
                SortBy::Created => entry.created.map(when).unwrap_or_default(),
                SortBy::Accessed => entry.accessed.map(when).unwrap_or_default(),
                SortBy::Kind => match &entry.trashed {
                    Some(item) => format!("From {}", item.original_parent.display()),
                    None => entry.describe.clone(),
                },
                SortBy::Name => String::new(),
            };
            let cell = Rect::from_min_size(pos2(x, rect.top()), vec2(*w, ROW));
            let (align, at) = if *right {
                (Align2::RIGHT_CENTER, cell.right_center() - vec2(8.0, 0.0))
            } else {
                (Align2::LEFT_CENTER, cell.left_center() + vec2(8.0, 0.0))
            };
            ui.painter_at(cell).text(at, align, text, font.clone(), weak);
            x += w;
        }
        self.item_input(ui, &resp, &entry, selected, None);
    }

    /// The folder in columns, as a Mac's Finder shows it: a column for each
    /// folder from where the path starts down to this one, each with the way
    /// down marked; this folder's column, the one with the keys; and right of
    /// it, the selected folder's items or the selected file's preview.
    fn column_view(&mut self, ui: &mut Ui, visible: &[usize]) {
        let Some(here) = self.current().folder().map(Path::to_path_buf) else { return };
        let chain = self.column_chain();
        let one = match self.current().selection()[..] {
            [entry] => Some(entry.clone()),
            _ => None,
        };
        let child = one.as_ref().filter(|e| e.is_folder()).map(|e| e.path.clone());
        let preview = one.filter(|e| !e.is_folder());
        let mut shown: Vec<PathBuf> = chain.iter().cloned().chain([here.clone()]).chain(child.clone()).collect();
        let moved = shown != self.shown_columns;
        let width = shown.len() as f32 * COLUMN + if preview.is_some() { PREVIEW } else { 0.0 };
        let px = (ROW_ICON * ui.ctx().pixels_per_point()).round() as u32;
        egui::ScrollArea::horizontal().auto_shrink(false).stick_to_right(true).id_salt(("columns", self.tab)).show(
            ui,
            |ui| {
                let (strip, _) = ui.allocate_exact_size(vec2(width, ui.available_height()), Sense::hover());
                let column = |n: usize| {
                    Rect::from_min_size(strip.min + vec2(n as f32 * COLUMN, 0.0), vec2(COLUMN, strip.height()))
                };
                for (n, folder) in chain.iter().enumerate() {
                    let next = chain.get(n + 1).unwrap_or(&here);
                    self.side_column(ui, column(n), folder, Some(next), px);
                }
                self.active_column(ui, column(chain.len()), &here, visible, px);
                let mut last = column(chain.len());
                if let Some(child) = &child {
                    last = column(chain.len() + 1);
                    self.side_column(ui, last, child, None, px);
                }
                if let Some(entry) = &preview {
                    last = Rect::from_min_size(
                        strip.min + vec2(shown.len() as f32 * COLUMN, 0.0),
                        vec2(PREVIEW, strip.height()),
                    );
                    self.preview_column(ui, last, entry);
                }
                if moved {
                    ui.scroll_to_rect(last, None);
                }
            },
        );
        self.shown_columns = std::mem::take(&mut shown);
    }

    /// The tab's own folder as a column: its items are the selection, the
    /// keys move in it, and the band selects in it.
    fn active_column(&mut self, ui: &mut Ui, rect: Rect, folder: &Path, visible: &[usize], px: u32) {
        column_edge(ui, rect);
        let mut area = egui::ScrollArea::vertical().auto_shrink(false).id_salt(("column", folder));
        if let Some(n) = self.scroll_target(visible) {
            area = area.vertical_scroll_offset(self.reveal(n as f32 * ROW, ROW));
        }
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            let output = area.show_rows(ui, ROW, visible.len(), |ui, range| {
                self.origin = pos2(ui.max_rect().left(), ui.min_rect().top() - range.start as f32 * ROW);
                for n in range {
                    let (row, _) = ui.allocate_exact_size(vec2(COLUMN, ROW), Sense::hover());
                    let entry = self.current().entries[visible[n]].clone();
                    let selected = self.current().selected.contains(&entry.path);
                    let mark = if selected { Mark::Selected } else { Mark::None };
                    self.column_row(ui, row, &entry, mark, None, px);
                }
            });
            self.viewport = (output.state.offset.y, output.inner_rect.height());
        });
    }

    /// Another folder's column: one to the left with `next`, the folder on
    /// the way down, marked; or the selected folder's, with nothing marked.
    /// Clicking in it makes its folder the tab's.
    fn side_column(&mut self, ui: &mut Ui, rect: Rect, folder: &Path, next: Option<&Path>, px: u32) {
        column_edge(ui, rect);
        let background = ui.interact(rect, Id::new(("column-background", folder)), Sense::click_and_drag());
        if background.clicked() || background.secondary_clicked() {
            self.actions.push(Action::Column { folder: folder.to_path_buf(), select: None });
        }
        let mut a = Vec::new();
        drop_target(ui, &background, &Location::Folder(folder.to_path_buf()), &mut a);
        self.actions.extend(a);

        let visible = match self.column_lists.get(folder) {
            Some(Ok(entries)) => shown(entries, self.config.show_hidden, "", self.current().types.as_ref()),
            Some(Err(e)) => {
                let text = egui::RichText::new(format!("Can't show this folder: {e}")).weak().small();
                ui.put(rect.shrink(12.0), egui::Label::new(text).wrap());
                return;
            }
            None => return,
        };
        let mut area = egui::ScrollArea::vertical().auto_shrink(false).id_salt(("column", folder));
        // A column that's new shows the way down in its middle.
        if !self.shown_columns.iter().any(|f| f == folder)
            && let Some(n) = self
                .column_entries(folder)
                .and_then(|entries| visible.iter().position(|&i| next.is_some_and(|next| entries[i].path == next)))
        {
            area = area.vertical_scroll_offset((n as f32 * ROW - rect.height() / 2.0).max(0.0));
        }
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            area.show_rows(ui, ROW, visible.len(), |ui, range| {
                for n in range {
                    let (row, _) = ui.allocate_exact_size(vec2(COLUMN, ROW), Sense::hover());
                    let Some(entry) = self.column_entries(folder).map(|e| e[visible[n]].clone()) else { return };
                    let mark = if next == Some(entry.path.as_path()) { Mark::OnTheWay } else { Mark::None };
                    self.column_row(ui, row, &entry, mark, Some(folder), px);
                }
            });
        });
    }

    fn column_entries(&self, folder: &Path) -> Option<&[Entry]> {
        self.column_lists.get(folder)?.as_deref().ok()
    }

    /// An item in a column: its icon and name, and an arrow if it's a folder
    /// to go into.
    fn column_row(&mut self, ui: &mut Ui, rect: Rect, entry: &Entry, mark: Mark, elsewhere: Option<&Path>, px: u32) {
        let resp = ui.interact(rect, Id::new(("item", &entry.path)), Sense::click_and_drag());
        let v = ui.visuals().clone();
        let painter = ui.painter_at(rect);
        let fill = rect.shrink2(vec2(4.0, 0.0));
        let dropping =
            entry.is_folder() && resp.dnd_hover_payload::<Dragged>().is_some_and(|d| !d.0.contains(&entry.path));
        let back = match mark {
            Mark::Selected => Some(v.selection.bg_fill),
            Mark::OnTheWay => Some(v.selection.bg_fill.gamma_multiply(0.35)),
            Mark::None if dropping || resp.hovered() => Some(v.widgets.hovered.weak_bg_fill),
            Mark::None => None,
        };
        if let Some(back) = back {
            painter.rect_filled(fill, CornerRadius::same(4), back);
        }
        let selected = mark == Mark::Selected;
        let colour = if selected { v.selection.stroke.color } else { v.text_color() };

        let icon =
            Rect::from_center_size(pos2(fill.left() + 6.0 + ROW_ICON / 2.0, rect.center().y), vec2(ROW_ICON, ROW_ICON));
        if let Some(texture) = self.icons.get(&self.icon_source(entry), px) {
            let dim = self.clip.as_ref().is_some_and(|c| c.cut && c.paths.contains(&entry.path));
            let tint = if dim { egui::Color32::WHITE.gamma_multiply(0.45) } else { egui::Color32::WHITE };
            painter.image(texture, icon, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
        }
        if entry.symlink {
            link_badge(&painter, icon, &v);
        }
        let arrow = Rect::from_min_max(pos2(fill.right() - 16.0, rect.top()), fill.right_bottom());
        if entry.is_folder() {
            glyphs::paint(&painter, arrow, Glyph::Separator, Stroke::new(1.0, colour.gamma_multiply(0.7)));
        }
        let text_left = icon.right() + 6.0;
        if elsewhere.is_none() && self.renaming.as_ref().is_some_and(|r| r.path == entry.path) {
            let field =
                Rect::from_min_max(pos2(text_left - 4.0, rect.top() + 1.0), pos2(arrow.left(), rect.bottom() - 1.0));
            self.rename_field(ui, field, false);
        } else {
            let mut job = LayoutJob::single_section(
                entry.name.clone(),
                TextFormat { font_id: egui::TextStyle::Body.resolve(ui.style()), color: colour, ..Default::default() },
            );
            job.wrap.max_width = arrow.left() - text_left;
            job.wrap.max_rows = 1;
            job.wrap.break_anywhere = true;
            let galley = ui.painter().layout_job(job);
            painter.galley(pos2(text_left, rect.center().y - galley.size().y / 2.0), galley, colour);
        }
        self.item_input(ui, &resp, entry, selected, elsewhere);
    }

    /// The selected file at the columns' end: large, with what it is.
    fn preview_column(&mut self, ui: &mut Ui, rect: Rect, entry: &Entry) {
        let px = (PREVIEW_ICON * ui.ctx().pixels_per_point()).round() as u32;
        let icon = self.icons.get(&self.icon_source(entry), px);
        let mut details = vec![entry.describe.clone()];
        if entry.kind == Kind::File && entry.remote.is_none() {
            details.push(listing::human_size(entry.size));
        }
        if let Some(modified) = entry.modified {
            details.push(format!("Modified {}", when(modified)));
        }
        let inner = rect.shrink2(vec2(16.0, 24.0));
        ui.scope_builder(UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(Align::Center)), |ui| {
            let (at, _) = ui.allocate_exact_size(vec2(PREVIEW_ICON, PREVIEW_ICON), Sense::hover());
            if let Some(icon) = icon {
                ui.painter().image(icon, at, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            ui.add_space(12.0);
            ui.add(egui::Label::new(egui::RichText::new(&entry.name).strong()).wrap());
            ui.add_space(4.0);
            for line in details {
                ui.label(egui::RichText::new(line).weak().small());
            }
        });
    }

    /// Clicks, drags, drops and the menu on one item; `elsewhere` is the
    /// folder of another column it's in, which a click makes the tab's.
    fn item_input(&mut self, ui: &Ui, resp: &Response, entry: &Entry, selected: bool, elsewhere: Option<&Path>) {
        let m = ui.input(|i| i.modifiers);
        if let Some(folder) = elsewhere
            && (resp.clicked() || resp.secondary_clicked())
        {
            self.actions.push(Action::Column { folder: folder.to_path_buf(), select: Some(entry.path.clone()) });
        } else if resp.clicked() {
            self.actions.push(Action::Click { path: entry.path.clone(), add: m.command, range: m.shift });
        }
        if resp.double_clicked() {
            self.actions.push(Action::Open(entry.path.clone()));
        }
        if resp.clicked_by(egui::PointerButton::Middle) && entry.is_folder() {
            self.actions.push(Action::OpenInNewTab(Location::Folder(entry.path.clone())));
        }
        if resp.secondary_clicked() && !selected && elsewhere.is_none() {
            self.actions.push(Action::SelectOnly(vec![entry.path.clone()]));
        }
        let tab = self.current();
        let in_trash = tab.in_trash();
        let remote = tab.remote();
        let count = if selected { tab.selected.len() } else { 1 };
        let selection: Vec<PathBuf> = tab.selection().iter().map(|e| e.path.clone()).collect();
        if resp.drag_started() && !in_trash && !remote {
            let paths = if selected {
                selection
            } else if elsewhere.is_some() {
                vec![entry.path.clone()]
            } else {
                self.actions.push(Action::SelectOnly(vec![entry.path.clone()]));
                vec![entry.path.clone()]
            };
            DragAndDrop::set_payload(ui.ctx(), Dragged(paths));
        }
        if entry.is_folder()
            && let Some(dragged) = resp.dnd_release_payload::<Dragged>()
            && !dragged.0.contains(&entry.path)
        {
            self.actions.push(Action::Drop { paths: dragged.0.clone(), into: Location::Folder(entry.path.clone()) });
        }

        let mut a = Vec::new();
        patina::menu::context(resp, |ui| {
            if let Some(item) = &entry.remote {
                if ui.button("Open").clicked() {
                    a.push(Action::Open(entry.path.clone()));
                }
                if ui.button("Copy Address").clicked() {
                    ui.ctx().copy_text(item.uri.clone());
                }
                return;
            }
            if in_trash {
                if ui.button("Put Back").clicked() {
                    a.push(Action::Restore);
                }
                if ui.button("Delete Permanently…").clicked() {
                    a.push(Action::Purge);
                }
                return;
            }
            if ui.button(if entry.script { "Run" } else { "Open" }).clicked() {
                a.push(Action::Open(entry.path.clone()));
            }
            if entry.script && ui.button("Edit").clicked() {
                a.push(Action::Edit(entry.path.clone()));
            }
            match entry.kind {
                Kind::App => {
                    if ui.button("Show Contents").clicked() {
                        a.push(Action::ShowContents(entry.path.clone()));
                    }
                }
                Kind::Folder => {
                    if ui.button("Open in New Tab").clicked() {
                        a.push(Action::OpenInNewTab(Location::Folder(entry.path.clone())));
                    }
                    if ui.button("Open Terminal Here").clicked() {
                        a.push(Action::Terminal(entry.path.clone()));
                    }
                    if self.config.favourites.contains(&entry.path) {
                        if ui.button("Remove from Favourites").clicked() {
                            a.push(Action::RemoveFavourite(entry.path.clone()));
                        }
                    } else if ui.button("Add to Favourites").clicked() {
                        a.push(Action::AddFavourite(entry.path.clone()));
                    }
                }
                Kind::File => {}
            }
            ui.separator();
            if ui.button("Cut").clicked() {
                a.push(Action::Cut);
            }
            if ui.button("Copy").clicked() {
                a.push(Action::Copy);
            }
            if ui.button("Copy Path").clicked() {
                a.push(Action::CopyPath);
            }
            ui.separator();
            if ui.add_enabled(count == 1, egui::Button::new("Rename…")).clicked() {
                a.push(Action::StartRename);
            }
            if ui.button("Move to Trash").clicked() {
                a.push(Action::Trash);
            }
            ui.separator();
            if ui.button("Properties").clicked() {
                a.push(Action::Properties);
            }
        });
        self.actions.extend(a);
    }

    /// The field an item's name is edited in: Enter or a click away keeps
    /// the new name, Escape the old.
    fn rename_field(&mut self, ui: &mut Ui, rect: Rect, centred: bool) {
        let Some(renaming) = &mut self.renaming else { return };
        let mut done = false;
        ui.scope_builder(UiBuilder::new().max_rect(rect), |ui| {
            let edit = egui::TextEdit::singleline(&mut renaming.text)
                .desired_width(rect.width())
                .horizontal_align(if centred { Align::Center } else { Align::LEFT });
            let mut output = edit.show(ui);
            let id = output.response.id;
            if !renaming.started {
                renaming.started = true;
                // The name without its extension is selected, ready to type over.
                let (stem, _) = ops::split_extension(&renaming.text);
                let stem = stem.chars().count();
                output.state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(stem))));
                output.state.store(ui.ctx(), id);
                output.response.request_focus();
            } else if output.response.lost_focus() {
                done = true;
            }
        });
        if done {
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                self.renaming = None;
            } else {
                self.finish_rename(ui.ctx());
            }
        }
    }

    /// The scroll offset that brings `top` to `top + height` into view,
    /// moving as little as it can.
    fn reveal(&self, top: f32, height: f32) -> f32 {
        let (offset, view) = self.viewport;
        if top < offset {
            top
        } else if top + height > offset + view {
            top + height - view
        } else {
            offset
        }
    }

    /// The visible position to scroll to, if the cursor moved.
    fn scroll_target(&mut self, visible: &[usize]) -> Option<usize> {
        let tab = &mut self.tabs[self.tab];
        let path = tab.scroll_to.take()?;
        visible.iter().position(|&i| tab.entries[i].path == path)
    }

    /// Files being dragged become a Wayland drag, so whatever app is under
    /// the pointer when they're let go can take them (ADR-0006).
    fn drag_out(&mut self, ctx: &egui::Context) {
        let Some(out) = &self.dnd else { return };
        if self.dragging_out {
            return;
        }
        let Some(dragged) = DragAndDrop::payload::<Dragged>(ctx) else { return };
        let source = self
            .current()
            .entries
            .iter()
            .find(|e| Some(&e.path) == dragged.0.first())
            .map(|e| self.icon_source(e))
            .unwrap_or_else(|| icons::theme(&["text-x-generic"]));
        // The icon already on screen if there is one, rather than decoding
        // a big picture again while the drag waits.
        let drawn = self.icons.drawn(&source);
        self.dragging_out = out.start(&dragged.0, ctx.pixels_per_point(), move |px| {
            drawn.map(|i| (*i).clone()).or_else(|| icons::image(&source, px))
        });
    }

    /// A Wayland drag over our window, ours or another app's, as the
    /// pointer egui doesn't get while the compositor has it: moves, and a
    /// release where it was dropped, or well away when ours went elsewhere.
    /// Another app's files are egui's payload while they're over us, so
    /// folders light up and take them as they do our own (ADR-0007).
    fn drag_over(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let Some(out) = &self.dnd else { return };
        let release = |pos: egui::Pos2, modifiers| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers,
        };
        for over in out.over() {
            match over {
                Over::Incoming(paths) => {
                    if !paths.is_empty() && !self.dragging_out {
                        DragAndDrop::set_payload(ctx, Dragged(paths));
                        self.incoming = true;
                    }
                }
                Over::Moved(pos) => raw_input.events.push(egui::Event::PointerMoved(pos)),
                Over::Left => {
                    raw_input.events.push(egui::Event::PointerGone);
                    if std::mem::take(&mut self.incoming) {
                        DragAndDrop::clear_payload(ctx);
                    }
                }
                Over::Dropped { at, copy } => {
                    raw_input.events.push(egui::Event::PointerMoved(at));
                    raw_input.events.push(release(at, raw_input.modifiers));
                    self.dropped_here = self.dragging_out;
                    self.drop_copy = copy;
                    self.incoming = false;
                }
                Over::Ended => {
                    if !self.dropped_here {
                        let away = egui::pos2(-1.0e4, -1.0e4);
                        raw_input.events.push(egui::Event::PointerMoved(away));
                        raw_input.events.push(release(away, raw_input.modifiers));
                        raw_input.events.push(egui::Event::PointerGone);
                    }
                    self.dragging_out = false;
                    self.dropped_here = false;
                }
            }
        }
    }

    /// What's being dragged, drawn beside the pointer.
    fn drag_ghost(&self, ctx: &egui::Context) {
        // The compositor draws a Wayland drag's icon.
        if self.dragging_out || self.incoming {
            return;
        }
        let Some(dragged) = DragAndDrop::payload::<Dragged>(ctx) else { return };
        let Some(pointer) = ctx.pointer_interact_pos() else { return };
        let text = match &dragged.0[..] {
            [one] => places::name(one),
            many => format!("{} items", many.len()),
        };
        let copy = ctx.input(|i| i.modifiers.ctrl);
        let text = if copy { format!("+ {text}") } else { text };
        let painter = ctx.layer_painter(LayerId::new(Order::Tooltip, Id::new("drag-ghost")));
        let v = ctx.global_style().visuals.clone();
        let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), v.selection.stroke.color);
        let rect = Rect::from_min_size(pointer + vec2(14.0, 10.0), galley.size()).expand2(vec2(8.0, 4.0));
        painter.rect_filled(rect, CornerRadius::same(6), v.selection.bg_fill);
        painter.galley(rect.min + vec2(8.0, 4.0), galley, v.selection.stroke.color);
        ctx.set_cursor_icon(if copy { egui::CursorIcon::Copy } else { egui::CursorIcon::Grabbing });
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        self.ask_dialog(ctx);
        self.conflict_dialog(ctx);
        let Some(confirm) = &self.confirm else { return };
        let (title, detail) = match confirm {
            Confirm::Purge(items) if items.len() == 1 => {
                (format!("Delete “{}” permanently?", items[0].name.to_string_lossy()), "It can't be put back.")
            }
            Confirm::Purge(items) => (format!("Delete {} items permanently?", items.len()), "They can't be put back."),
            Confirm::EmptyTrash => ("Empty the Trash?".to_string(), "Everything in it is deleted for good."),
            Confirm::Replace(path) => {
                (format!("Replace “{}”?", places::name(path)), "A file with that name is already there.")
            }
        };
        let yes = if matches!(confirm, Confirm::Replace(_)) { "Replace" } else { "Delete" };
        let mut answer = None;
        let modal = egui::Modal::new(Id::new("confirm")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading(title);
            ui.label(detail);
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.button(yes).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
        });
        if modal.should_close() {
            answer = answer.or(Some(false));
        }
        match answer {
            Some(true) if matches!(self.confirm, Some(Confirm::Replace(_))) => {
                // Accepting again finds the confirmation and answers.
                self.accept(ctx);
                self.confirm = None;
            }
            Some(true) => {
                let op = match self.confirm.take() {
                    Some(Confirm::Purge(items)) => Op::Purge(items),
                    _ => Op::Purge(trash::os_limited::list().unwrap_or_default()),
                };
                self.worker.start(op);
            }
            Some(false) => self.confirm = None,
            None => {}
        }
    }

    /// A name taken where a copy or move is going, asked about as Nautilus
    /// does (ADR-0009): two folders merge, anything else is replaced, and
    /// either can keep both or be skipped.
    fn conflict_dialog(&mut self, ctx: &egui::Context) {
        let conflict = self.worker.status.lock().unwrap_or_else(|e| e.into_inner()).conflict.clone();
        let Some(conflict) = conflict else { return };
        let name = places::name(&conflict.existing);
        let into = conflict.existing.parent().map(places::name).unwrap_or_default();
        let (title, detail, yes, choice, all) = if conflict.folders {
            (
                format!("Merge folder “{name}”?"),
                format!(
                    "A folder with that name is already in “{into}”. Merging asks before replacing any files in it."
                ),
                "Merge",
                Choice::Merge,
                "Do this for all folders",
            )
        } else {
            (
                format!("Replace “{name}”?"),
                format!("An item with that name is already in “{into}”. Replacing puts it in the Trash."),
                "Replace",
                Choice::Replace,
                "Do this for all files",
            )
        };
        let mut answer = None;
        let modal = egui::Modal::new(Id::new(("conflict", &conflict.source))).show(ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(title);
            ui.label(detail);
            ui.add_space(8.0);
            egui::Grid::new("conflict sides").num_columns(2).show(ui, |ui| {
                // The conflict's sides are the newcomer's, then what's there.
                for (side, i) in [("Already there", 1), ("Coming in", 0)] {
                    let about: Vec<String> =
                        [conflict.sizes[i].map(listing::human_size), conflict.modified[i].map(when)]
                            .into_iter()
                            .flatten()
                            .collect();
                    ui.label(egui::RichText::new(side).weak());
                    ui.label(about.join(", "));
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.checkbox(&mut self.for_all, all);
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if ui.button(yes).clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                    answer = Some(choice);
                }
                if ui.button("Keep Both").clicked() {
                    answer = Some(Choice::KeepBoth);
                }
                if ui.button("Skip").clicked() {
                    answer = Some(Choice::Skip);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(Choice::Cancel);
                }
            });
        });
        if modal.should_close() {
            answer = answer.or(Some(Choice::Cancel));
        }
        if let Some(choice) = answer {
            self.worker.answer(choice, std::mem::take(&mut self.for_all));
        }
    }

    // ---- Properties ----

    /// Properties (Ctrl+I): what's selected, or the folder, as Nautilus
    /// shows them: type, size and where it is, its times, and who may read,
    /// change or run it, which its owner can change here.
    /// The right panel: the dataset's space, its pool and its snapshots.
    fn zfs_side_panel(&mut self, ui: &mut Ui) {
        let Some((_, _, Some(info))) = &self.zfs else { return };
        let details = self.zfs_details.as_ref().map(|(_, d)| d);
        let size = listing::human_size;
        let mut close = false;
        ui.horizontal(|ui| {
            ui.heading("ZFS");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                close = glyph_button(ui, Glyph::Close, true, "Close").clicked();
            });
        });
        ui.label(egui::RichText::new(&info.dataset).strong());
        ui.add_space(6.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let grid = |ui: &mut Ui, id: &str, rows: Vec<(String, String)>| {
                egui::Grid::new(id).num_columns(2).spacing([12.0, 3.0]).show(ui, |ui| {
                    for (k, v) in rows {
                        ui.label(egui::RichText::new(k).weak());
                        ui.add(egui::Label::new(v).wrap());
                        ui.end_row();
                    }
                });
            };
            ui.label(egui::RichText::new("Space").small().weak());
            let space = info.quota.unwrap_or(info.used + info.available);
            if space > 0 {
                ui.add(egui::ProgressBar::new(info.used as f32 / space as f32));
                ui.label(egui::RichText::new(format!("{} of {}", size(info.used), size(space))).small());
            }
            let mut rows = vec![
                ("Used".to_string(), size(info.used)),
                ("Available".to_string(), size(info.available)),
                ("Referenced".to_string(), size(info.referenced)),
            ];
            if let Some(quota) = info.quota {
                rows.push(("Quota".into(), size(quota)));
            }
            if let Some(d) = details {
                rows.push(("In snapshots".into(), size(d.by_snapshots)));
                rows.push(("In children".into(), size(d.by_children)));
                if d.logical > 0 {
                    rows.push(("Before compression".into(), size(d.logical)));
                }
            }
            rows.push(("Compression".into(), format!("{}, {}× saved", info.compression, info.ratio)));
            if info.recordsize > 0 {
                rows.push(("Record size".into(), size(info.recordsize)));
            }
            grid(ui, "zfs_space", rows);

            let Some(d) = details else {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Reading…");
                });
                return;
            };
            if let Some(pool) = &d.pool {
                ui.add_space(10.0);
                ui.label(egui::RichText::new(format!("Pool {}", pool.name)).small().weak());
                let mut rows = vec![
                    ("Health".to_string(), pool.health.clone()),
                    ("Size".into(), size(pool.size)),
                    ("Allocated".into(), size(pool.allocated)),
                    ("Free".into(), size(pool.free)),
                ];
                if let Some(f) = pool.fragmentation {
                    rows.push(("Fragmentation".into(), format!("{f}%")));
                }
                rows.push(("Dedup ratio".into(), format!("{}×", pool.dedup)));
                if let Some(scan) = &d.scan {
                    rows.push(("Scan".into(), scan.clone()));
                }
                grid(ui, "zfs_pool", rows);
            }
            if !d.properties.is_empty() {
                ui.add_space(10.0);
                ui.label(egui::RichText::new("Properties").small().weak());
                grid(ui, "zfs_props", d.properties.clone());
            }
            ui.add_space(10.0);
            let n = d.snapshots.len();
            ui.label(egui::RichText::new(format!("Snapshots ({n})")).small().weak());
            if n == 0 {
                ui.label(egui::RichText::new("None").weak());
            }
            // Newest first.
            for snap in d.snapshots.iter().rev() {
                let created = std::time::UNIX_EPOCH + Duration::from_secs(snap.created.max(0) as u64);
                ui.add_space(2.0);
                ui.add(egui::Label::new(egui::RichText::new(&snap.name).strong()).wrap());
                ui.label(
                    egui::RichText::new(format!(
                        "{} · {} unique · {} referenced",
                        date_time(created),
                        size(snap.used),
                        size(snap.referenced)
                    ))
                    .small()
                    .weak(),
                );
            }
        });
        if close {
            self.config.zfs_panel = false;
            self.config.save();
        }
    }

    fn properties_window(&mut self, ctx: &egui::Context) {
        let Some(properties) = &self.properties else { return };
        let info = properties.info().clone();
        let px = (PROPERTIES_ICON * ctx.pixels_per_point()).round() as u32;
        let one = match &info.items[..] {
            [item] => Some(item),
            _ => None,
        };
        let icon = one.and_then(|item| self.icons.get(&self.icon_source(&item.entry), px));
        let title = one.map_or_else(|| "Properties".to_string(), |item| format!("{} Properties", item.entry.name));
        let mut open = true;
        let mut mode = None;
        egui::Window::new(title)
            .id(Id::new("properties"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                ui.set_min_width(360.0);
                if !info.read {
                    ui.spinner();
                    return;
                }
                for error in &info.errors {
                    ui.label(egui::RichText::new(error).color(ui.visuals().error_fg_color));
                }
                if info.items.is_empty() {
                    return;
                }
                let name = one.map_or_else(|| format!("{} items", info.items.len()), |item| item.entry.name.clone());
                ui.horizontal(|ui| {
                    if let Some(icon) = icon {
                        ui.image((icon, vec2(PROPERTIES_ICON, PROPERTIES_ICON)));
                    }
                    ui.add(egui::Label::new(egui::RichText::new(name).heading()).wrap());
                });
                ui.add_space(8.0);

                let weak = |ui: &mut Ui, text: &str| {
                    ui.label(egui::RichText::new(text).weak());
                };
                egui::Grid::new("properties details").num_columns(2).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                    let first = &info.items[0];
                    let kinds: HashSet<&str> = info.items.iter().map(|i| i.entry.describe.as_str()).collect();
                    let kind = match (kinds.len(), &first.entry.mime) {
                        (1, Some(mime)) if one.is_some() => format!("{} ({mime})", first.entry.describe),
                        (1, _) => first.entry.describe.clone(),
                        _ => "Mixed".into(),
                    };
                    weak(ui, "Type");
                    ui.label(kind);
                    ui.end_row();

                    weak(ui, if one.is_some_and(properties::Item::is_folder) { "Contents" } else { "Size" });
                    ui.horizontal(|ui| {
                        ui.label(info.size());
                        if info.counting && one.is_none_or(properties::Item::is_folder) {
                            ui.spinner();
                        }
                    });
                    ui.end_row();
                    if info.unreadable > 0 {
                        ui.label("");
                        let folders = if info.unreadable == 1 { "folder" } else { "folders" };
                        ui.label(
                            egui::RichText::new(format!("{} {folders} inside couldn't be read", info.unreadable))
                                .color(ui.visuals().warn_fg_color),
                        );
                        ui.end_row();
                    }

                    let parents: HashSet<Option<&Path>> = info.items.iter().map(|i| i.entry.path.parent()).collect();
                    if let [Some(parent)] = parents.into_iter().collect::<Vec<_>>()[..] {
                        weak(ui, "Location");
                        ui.add(egui::Label::new(parent.display().to_string()).selectable(true));
                        ui.end_row();
                    }
                    let Some(item) = one else { return };
                    if let Some(target) = &item.target {
                        weak(ui, "Link to");
                        ui.add(egui::Label::new(target.display().to_string()).selectable(true));
                        ui.end_row();
                    }
                    if let Some((free, total)) = item.disk {
                        weak(ui, "Free space");
                        ui.label(format!("{} of {}", listing::human_size(free), listing::human_size(total)));
                        ui.end_row();
                    }
                    for (label, time) in
                        [("Modified", item.entry.modified), ("Accessed", item.accessed), ("Created", item.created)]
                    {
                        if let Some(time) = time {
                            weak(ui, label);
                            ui.label(date_time(time));
                            ui.end_row();
                        }
                    }
                });

                let Some(item) = one else { return };
                if item.entry.remote.is_some() {
                    return;
                }
                ui.add_space(4.0);
                ui.separator();
                let folder = item.is_folder();
                let mine = properties::may_change(item);
                ui.add_enabled_ui(mine, |ui| {
                    egui::Grid::new("properties permissions").num_columns(3).spacing(vec2(12.0, 6.0)).show(ui, |ui| {
                        for (who, name, shift) in
                            [("Owner", item.owner.as_str(), 6), ("Group", item.group.as_str(), 3), ("Others", "", 0)]
                        {
                            weak(ui, who);
                            ui.label(name);
                            let (bits, shown) = properties::access(item.mode, shift, folder);
                            egui::ComboBox::from_id_salt(("access", shift)).selected_text(shown).width(180.0).show_ui(
                                ui,
                                |ui| {
                                    for &(level, value) in properties::levels(folder).0 {
                                        if ui.selectable_label(bits == value, level).clicked() {
                                            mode = Some(properties::with_access(item.mode, shift, folder, value));
                                        }
                                    }
                                },
                            );
                            ui.end_row();
                        }
                    });
                    if !folder {
                        let mut executable = item.mode & 0o111 != 0;
                        if ui.checkbox(&mut executable, "Allow executing file as program").changed() {
                            mode = Some(properties::with_executable(item.mode, executable));
                        }
                    }
                });
                if !mine {
                    weak(ui, "Only its owner can change who may use it.");
                }
            });
        if !open {
            self.properties = None;
        } else if let (Some(mode), Some(properties)) = (mode, &self.properties) {
            properties.set_mode(mode);
        }
    }

    // ---- Quick Look ----

    /// What Quick Look shows: the item at the cursor, or the first selected.
    fn looked_at(&self) -> Option<&Entry> {
        let tab = self.current();
        match tab.cursor.as_ref().filter(|c| tab.selected.contains(*c)) {
            Some(cursor) => tab.entries.iter().find(|e| &e.path == cursor),
            None => tab.entries.iter().find(|e| tab.selected.contains(&e.path)),
        }
    }

    /// Quick Look (ADR-0008): the item at the cursor in a window of its
    /// own, a picture drawn to fit and anything else as its icon. The folder's
    /// keys work in it too, so the arrow keys step through the folder and the
    /// window follows.
    fn quick_look(&mut self, ctx: &egui::Context) {
        // A name being edited (F2, a new folder) wants the keys back.
        if self.renaming.is_some() {
            self.looking = false;
        }
        if !self.looking {
            return;
        }
        let Some(entry) = self.looked_at().cloned() else {
            self.looking = false;
            return;
        };
        let custom = !self.config.system_title_bar;
        let builder = frame::viewport(
            egui::ViewportBuilder::default()
                .with_title(entry.name.clone())
                .with_app_id(QUICK_LOOK_ID)
                .with_inner_size(QUICK_LOOK_SIZE)
                .with_min_inner_size([320.0, 240.0]),
            custom,
        );
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(QUICK_LOOK_ID), builder, |ui, _| {
            self.quick_look_window(ui, &entry, custom)
        });
    }

    fn quick_look_window(&mut self, ui: &mut Ui, entry: &Entry, custom: bool) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.viewport().close_requested()) {
            self.looking = false;
            return;
        }
        self.keys(&ctx);
        let style = self.skins[self.skin].frame_style(ui.visuals());
        let radius = if custom { frame::corner_radius(&ctx, style.radius) } else { 0 };
        let ppp = ctx.pixels_per_point();
        let icon = self.icons.get(&self.icon_source(entry), (QUICK_LOOK_ICON * ppp).round() as u32);
        // Decoded at the screen's size, so a maximised window is sharp.
        let screen = ctx.input(|i| i.viewport().monitor_size).map_or(2048.0, |s| s.max_elem() * ppp);
        let mut px = screen.round().clamp(1024.0, 4096.0) as u32;
        if entry.mime.as_deref() == Some("image/svg+xml") {
            // Drawn for the window instead, having no size of its own: in
            // steps, so resizing doesn't draw it again each frame. Kept to
            // 1024: resvg takes most of a second for a large one.
            let window = ctx.content_rect().size().max_elem() * ppp;
            px = ((window / 256.0).ceil() as u32 * 256).clamp(256, 1024);
        }
        let picture = entry.kind == Kind::File
            && entry.remote.is_none()
            && entry.mime.as_deref().is_some_and(icons::has_thumbnail);
        // None while it's loading; an error shows the icon instead.
        let shown = picture.then(|| match self.previews.get(&entry.path, entry.modified, px) {
            preview::State::Loading => None,
            preview::State::Ready(p) => Some(Ok((p.texture.id(), p.drawn, p.size))),
            preview::State::Failed(e) => Some(Err(e.to_string())),
        });

        let mut details = vec![entry.describe.clone()];
        match entry.kind {
            Kind::Folder => {
                details.push(if entry.size == 1 { "1 item".into() } else { format!("{} items", entry.size) })
            }
            Kind::File => details.push(listing::human_size(entry.size)),
            Kind::App => {}
        }
        if let Some(Some(Ok((_, _, Some([w, h]))))) = &shown {
            details.push(format!("{w} × {h}"));
        }
        if let Some(modified) = entry.modified {
            details.push(when(modified));
        }

        let mut a = Vec::new();
        let header = frame::header_frame(ui.style(), &style, radius);
        egui::Panel::top("quick-look-header").frame(header).show(ui, |ui| {
            TitleBar::new(&entry.name, &style).custom(custom).show(
                ui,
                |_| {},
                |ui| {
                    if ui.button("Open").on_hover_text("Enter").clicked() {
                        a.push(Action::Open(entry.path.clone()));
                    }
                },
            );
        });
        egui::Panel::bottom("quick-look-details")
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 4))
                    .corner_radius(CornerRadius { sw: radius, se: radius, ..Default::default() }),
            )
            .show(ui, |ui| {
                ui.add(egui::Label::new(egui::RichText::new(details.join(" · ")).weak()).truncate());
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::same(8)))
            .show(ui, |ui| {
                let body = ui.available_rect_before_wrap();
                match &shown {
                    Some(None) => {
                        // Its icon (the folder's thumbnail) until it's ready.
                        let at = Rect::from_center_size(body.center(), vec2(QUICK_LOOK_ICON, QUICK_LOOK_ICON));
                        if let Some(icon) = icon {
                            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                            ui.painter().image(icon, at, uv, egui::Color32::WHITE);
                        }
                        let below = Rect::from_center_size(at.center_bottom() + vec2(0.0, 24.0), vec2(20.0, 20.0));
                        ui.put(below, egui::Spinner::new());
                    }
                    Some(Some(Ok((texture, drawn, size)))) => {
                        // At its own size, or smaller to fit; never larger.
                        let natural = size.map_or(*drawn, |[w, h]| vec2(w as f32, h as f32)) / ppp;
                        let scale = (body.width() / natural.x).min(body.height() / natural.y).min(1.0);
                        let shown = Rect::from_center_size(body.center(), natural * scale);
                        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                        ui.painter().image(*texture, shown, uv, egui::Color32::WHITE);
                    }
                    _ => {
                        let failed = match &shown {
                            Some(Some(Err(e))) => Some(e.as_str()),
                            _ => None,
                        };
                        quick_look_card(ui, body, icon, &entry.name, failed);
                    }
                }
            });
        self.actions.extend(a);
        if custom {
            frame::edges(&ctx, radius, style.border);
        }
    }

    /// A server's question: a password, as Nautilus asks for one, or a
    /// choice such as whether to trust a host.
    fn ask_dialog(&mut self, ctx: &egui::Context) {
        let Some(asking) = self.asking.first_mut() else { return };
        let mut answer = None;
        let modal = egui::Modal::new(Id::new(("ask", asking.ask.id))).show(ctx, |ui| {
            ui.set_width(380.0);
            let mut lines = asking.ask.message.lines().filter(|l| !l.trim().is_empty());
            ui.heading(lines.next().unwrap_or("Connect"));
            for line in lines {
                ui.label(line);
            }
            ui.add_space(8.0);
            match &asking.ask.kind {
                network::AskKind::Password { need_user, need_domain, need_password, anonymous, can_save, .. } => {
                    if *anonymous {
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut asking.anonymous, false, "Registered user");
                            ui.radio_value(&mut asking.anonymous, true, "Anonymous");
                        });
                        ui.add_space(4.0);
                    }
                    ui.add_enabled_ui(!asking.anonymous, |ui| {
                        // The keys go to the first field that wants something.
                        let mut focus = !std::mem::replace(&mut asking.focused, true);
                        egui::Grid::new("credentials").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                            let mut field = |ui: &mut Ui, label: &str, text: &mut String, password: bool| {
                                ui.label(label);
                                let resp =
                                    ui.add(egui::TextEdit::singleline(text).password(password).desired_width(260.0));
                                if focus && (text.is_empty() || password) {
                                    resp.request_focus();
                                    focus = false;
                                }
                                ui.end_row();
                            };
                            if *need_user {
                                field(ui, "Username", &mut asking.user, false);
                            }
                            if *need_domain {
                                field(ui, "Domain", &mut asking.domain, false);
                            }
                            if *need_password {
                                field(ui, "Password", &mut asking.password, true);
                            }
                        });
                        if *can_save {
                            ui.add_space(6.0);
                            ui.radio_value(&mut asking.save, network::Save::Never, "Forget password immediately");
                            ui.radio_value(&mut asking.save, network::Save::ForSession, "Remember until you log out");
                            ui.radio_value(&mut asking.save, network::Save::Permanently, "Remember forever");
                        }
                    });
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Connect").clicked() || ui.input(|i| i.key_pressed(Key::Enter)) {
                            answer = Some(network::Answer::Password {
                                anonymous: asking.anonymous,
                                user: asking.user.clone(),
                                domain: asking.domain.clone(),
                                password: std::mem::take(&mut asking.password),
                                save: asking.save,
                            });
                        }
                        if ui.button("Cancel").clicked() {
                            answer = Some(network::Answer::Cancel);
                        }
                    });
                }
                network::AskKind::Question(choices) => {
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        for (i, choice) in choices.iter().enumerate().rev() {
                            if ui.button(choice).clicked() {
                                answer = Some(network::Answer::Choice(i));
                            }
                        }
                    });
                }
            }
        });
        if modal.should_close() {
            answer = answer.or(Some(network::Answer::Cancel));
        }
        if let Some(answer) = answer {
            let asking = self.asking.remove(0);
            if matches!(answer, network::Answer::Cancel) && self.asking.is_empty() {
                self.connecting = None;
            }
            self.network.answer(asking.ask.id, answer);
        }
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.picking {
            Some(picking) => picking.request.title(),
            None => Self::title_of(&self.current().location, &self.mounts),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }

    /// In screenshot mode: waits for the listing and icons, saves the
    /// window to `FILES_SCREENSHOT` and quits.
    fn take_screenshot(&mut self, ctx: &egui::Context) {
        let Some((path, requested)) = &mut self.screenshot else { return };
        ctx.request_repaint();
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let [w, h] = image.size;
            let saved = image::RgbaImage::from_raw(w as u32, h as u32, image.as_raw().to_vec())
                .ok_or_else(|| "the screenshot has the wrong size".to_string())
                .and_then(|buf| buf.save(&*path).map_err(|e| e.to_string()));
            match saved {
                Ok(()) => eprintln!("saved {}", path.display()),
                Err(e) => eprintln!("files: screenshot failed: {e}"),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        } else if !*requested && ctx.input(|i| i.time) > 2.0 {
            *requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
    }
}

impl eframe::App for Files {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        let custom = !self.config.system_title_bar;
        let style = self.skins[self.skin].frame_style(ui.visuals());
        let radius = if custom { frame::corner_radius(&ctx, style.radius) } else { 0 };

        if self.picking.is_none()
            && let Some(action) = tabs::keys(&ctx, self.tabs.len(), self.tab)
        {
            // Ctrl+W on the last tab closes the window, as Close Tab does.
            self.actions.push(match action {
                TabAction::Close(_) => Action::CloseTab,
                action => Action::Tab(action),
            });
        }
        self.keys(&ctx);

        let header = frame::header_frame(ui.style(), &style, radius);
        egui::Panel::top("header").frame(header).show(ui, |ui| {
            let span = (ui.max_rect() + header.inner_margin).y_range();
            self.header(ui, &style, span);
        });
        egui::Panel::top("toolbar")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(8, 6)))
            .show(ui, |ui| self.toolbar(ui));
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin::symmetric(10, 4))
                    .corner_radius(CornerRadius { sw: radius, se: radius, ..Default::default() }),
            )
            .show(ui, |ui| if self.picking.is_some() { self.chooser_bar(ui) } else { self.status_bar(ui) });
        egui::Panel::left("sidebar")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(8, 0)))
            .default_size(180.0)
            .size_range(120.0..=320.0)
            .show(ui, |ui| self.sidebar(ui));
        if self.config.zfs_panel && self.picking.is_none() && matches!(&self.zfs, Some((_, _, Some(_)))) {
            egui::Panel::right("zfs")
                .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 8)))
                .default_size(300.0)
                .size_range(220.0..=520.0)
                .show(ui, |ui| self.zfs_side_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::same(4)))
            .show(ui, |ui| self.items(ui));

        self.quick_look(&ctx);
        self.properties_window(&ctx);
        self.drag_out(&ctx);
        self.drag_ghost(&ctx);
        self.dialogs(&ctx);
        for action in std::mem::take(&mut self.actions) {
            self.apply(action, &ctx);
        }
        // Only for a drop made this frame.
        self.drop_copy = false;
        self.update_title(&ctx);
        if custom {
            frame::edges(&ctx, radius, style.border);
        }
        self.take_screenshot(&ctx);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        frame::clear_color(visuals, !self.config.system_title_bar)
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        frame::release_grab(ctx, raw_input);
        self.drag_over(ctx, raw_input);
        patina::ime::drop_repeated_preedits(ctx, raw_input);
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }
}

/// A message for the status bar.
struct Message {
    level: Level,
    text: String,
    /// Shown on hover: what a summary stands for.
    detail: Vec<String>,
}

enum Level {
    Error,
    Warn,
    Info,
}

impl Message {
    fn error(text: impl Into<String>) -> Self {
        Message { level: Level::Error, text: text.into(), detail: Vec::new() }
    }

    fn warn(text: impl Into<String>) -> Self {
        Message { level: Level::Warn, text: text.into(), detail: Vec::new() }
    }

    fn info(text: impl Into<String>) -> Self {
        Message { level: Level::Info, text: text.into(), detail: Vec::new() }
    }

    /// Small, red for errors, yellow for warnings and weak otherwise.
    fn text(&self, ui: &Ui) -> egui::RichText {
        let colour = match self.level {
            Level::Error => ui.visuals().error_fg_color,
            Level::Warn => ui.visuals().warn_fg_color,
            Level::Info => ui.visuals().weak_text_color(),
        };
        egui::RichText::new(&self.text).color(colour).small()
    }
}

fn load_skins() -> (Vec<Skin>, Vec<String>) {
    skin::load_all(skin::BUILT_IN, &skin::app_dir(APP).into_iter().collect::<Vec<_>>())
}

/// The columns the list can show after Name, in the order they appear.
const OPTIONAL_COLUMNS: [(SortBy, &str); 5] = [
    (SortBy::Size, "Size"),
    (SortBy::Modified, "Date Modified"),
    (SortBy::Created, "Date Created"),
    (SortBy::Accessed, "Date Last Opened"),
    (SortBy::Kind, "Kind"),
];

/// The list's columns: what each sorts by, its heading, width and whether
/// it's right-aligned. Name takes what the others leave; of those `wanted`,
/// the last go first when the window is narrow.
fn list_columns(width: f32, wanted: &[SortBy]) -> Vec<(SortBy, &'static str, f32, bool)> {
    let mut rest: Vec<(SortBy, &'static str, f32, bool)> = Vec::new();
    let mut used = 0.0;
    for (by, _) in OPTIONAL_COLUMNS.iter().filter(|(by, _)| wanted.contains(by)) {
        let (label, w, right) = match by {
            SortBy::Size => ("Size", 90.0, true),
            SortBy::Modified => ("Modified", 150.0, false),
            SortBy::Created => ("Created", 150.0, false),
            SortBy::Accessed => ("Last Opened", 150.0, false),
            _ => ("Kind", 200.0, false),
        };
        if width - used - w > 180.0 {
            rest.push((*by, label, w, right));
            used += w;
        }
    }
    let mut columns = vec![(SortBy::Name, "Name", (width - used).max(120.0), false)];
    columns.extend(rest);
    columns
}

/// The entries shown: hidden ones only when asked, only files of `types`,
/// and only those `filter` matches, as indices into `entries`.
fn shown(entries: &[Entry], show_hidden: bool, filter: &str, types: Option<&Filter>) -> Vec<usize> {
    let filter = filter.trim().to_lowercase();
    (0..entries.len())
        .filter(|&i| {
            let e = &entries[i];
            let typed = e.kind != Kind::File || types.is_none_or(|t| t.matches(&e.name, e.mime.as_deref()));
            typed && (show_hidden || !e.hidden) && (filter.is_empty() || e.name.to_lowercase().contains(&filter))
        })
        .collect()
}

/// How an item in a column is marked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    None,
    Selected,
    /// The folder the next column shows.
    OnTheWay,
}

/// The line between a column and the next.
fn column_edge(ui: &Ui, rect: Rect) {
    ui.painter().vline(rect.right(), rect.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
}

/// The path as places to go: from Home when it's inside it, from the
/// network in a share, else from the computer's root.
fn crumbs(location: &Location, mounts: &[network::Mount]) -> Vec<(String, Location)> {
    let network = || ("Network".to_string(), Location::Network(network::ROOT.into()));
    let path = match location {
        Location::Trash => return vec![("Trash".into(), Location::Trash)],
        Location::Network(uri) if uri == network::ROOT => return vec![network()],
        Location::Network(uri) => {
            return vec![network(), (network::host(uri).unwrap_or(uri).to_string(), location.clone())];
        }
        Location::Folder(path) => path,
    };
    if let Some(mount) = mounts.iter().find(|m| path.starts_with(&m.path)) {
        let mut at = mount.path.clone();
        let mut crumbs = vec![network(), (mount.name.clone(), Location::Folder(at.clone()))];
        for part in path.strip_prefix(&mount.path).unwrap_or(Path::new("")).components() {
            at.push(part);
            crumbs.push((part.as_os_str().to_string_lossy().into_owned(), Location::Folder(at.clone())));
        }
        return crumbs;
    }
    let home = dirs::home_dir();
    let (mut at, mut crumbs, rest) = match home.as_ref().and_then(|h| path.strip_prefix(h).ok().map(|r| (h, r))) {
        Some((home, rest)) => {
            (home.clone(), vec![("Home".to_string(), Location::Folder(home.clone()))], rest.to_path_buf())
        }
        None => (
            PathBuf::from("/"),
            vec![("Computer".to_string(), Location::Folder(PathBuf::from("/")))],
            path.strip_prefix("/").unwrap_or(path).to_path_buf(),
        ),
    };
    for part in rest.components() {
        at.push(part);
        crumbs.push((part.as_os_str().to_string_lossy().into_owned(), Location::Folder(at.clone())));
    }
    crumbs
}

/// A typed location: `~` is home.
fn expand(text: &str) -> PathBuf {
    let text = text.trim();
    if let Some(path) = uri::to_path(text) {
        return path;
    }
    let home = dirs::home_dir().unwrap_or_default();
    match text.strip_prefix('~') {
        Some(rest) => home.join(rest.trim_start_matches('/')),
        None => home.join(text),
    }
}

/// When a file changed: the time today, the day this week, else the date.
fn when(time: std::time::SystemTime) -> String {
    let secs = time.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let offset = local_offset();
    let (day, then) = ((secs + offset).div_euclid(86400), (secs + offset).rem_euclid(86400));
    let today = (now + offset).div_euclid(86400);
    let clock = format!("{:02}:{:02}", then / 3600, then % 3600 / 60);
    if day == today {
        return format!("Today {clock}");
    }
    if day == today - 1 {
        return format!("Yesterday {clock}");
    }
    let (y, m, d) = civil(day);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let (this_year, _, _) = civil(today);
    if y == this_year {
        format!("{d} {} {clock}", MONTHS[m as usize - 1])
    } else {
        format!("{d} {} {y}", MONTHS[m as usize - 1])
    }
}

/// The date and time in full, as Properties shows them: "24 Oct 2026 14:03".
fn date_time(time: std::time::SystemTime) -> String {
    let secs = time.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64) + local_offset();
    let (day, then) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (y, m, d) = civil(day);
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    format!("{d} {} {y} {:02}:{:02}", MONTHS[m as usize - 1], then / 3600, then % 3600 / 60)
}

/// Days since 1970 as a year, month and day (Howard Hinnant's algorithm).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The local time zone's offset from UTC in seconds, as the C library
/// knows it.
fn local_offset() -> i64 {
    static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        let out = std::process::Command::new("date").arg("+%z").output();
        let text = out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
        let sign = if text.starts_with('-') { -1 } else { 1 };
        let digits = text.trim_start_matches(['+', '-']);
        match (
            digits.get(0..2).and_then(|h| h.parse::<i64>().ok()),
            digits.get(2..4).and_then(|m| m.parse::<i64>().ok()),
        ) {
            (Some(h), Some(m)) => sign * (h * 3600 + m * 60),
            _ => 0,
        }
    })
}

fn shortcut(ui: &Ui, text: &str) -> egui::RichText {
    egui::RichText::new(text).color(ui.visuals().weak_text_color())
}

/// Quick Look for what isn't a picture: its icon and name in the middle of
/// `body`, with why a picture couldn't be shown.
fn quick_look_card(ui: &mut Ui, body: Rect, icon: Option<egui::TextureId>, name: &str, failed: Option<&str>) {
    let card = Rect::from_center_size(body.center(), vec2(body.width().min(420.0), QUICK_LOOK_ICON + 60.0));
    ui.scope_builder(UiBuilder::new().max_rect(card).layout(egui::Layout::top_down(Align::Center)), |ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(QUICK_LOOK_ICON, QUICK_LOOK_ICON), Sense::hover());
        if let Some(icon) = icon {
            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            ui.painter().image(icon, rect, uv, egui::Color32::WHITE);
        }
        ui.add_space(8.0);
        ui.add(egui::Label::new(egui::RichText::new(name).heading()).wrap());
        if let Some(e) = failed {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(format!("Can't preview it: {e}")).weak().small());
        }
    });
}

/// The dataset and its free space, with the rest on hover; a click opens the side panel.
fn zfs_label(ui: &mut Ui, info: &zfs::Info, open: bool) -> Response {
    let space = info.quota.unwrap_or(info.used + info.available);
    let text =
        format!("ZFS {} · {} of {} used", info.dataset, listing::human_size(info.used), listing::human_size(space));
    let mut hover = vec![
        format!("Dataset: {}", info.dataset),
        format!(
            "Used: {} (this dataset alone refers to {})",
            listing::human_size(info.used),
            listing::human_size(info.referenced)
        ),
        format!("Available: {}", listing::human_size(info.available)),
    ];
    if let Some(quota) = info.quota {
        hover.push(format!("Quota: {}", listing::human_size(quota)));
    }
    hover.push(format!("Compression: {}, {}× saved", info.compression, info.ratio));
    if info.recordsize > 0 {
        hover.push(format!("Record size: {}", listing::human_size(info.recordsize)));
    }
    hover.push(format!("Click to {} the ZFS panel", if open { "close" } else { "open" }));
    ui.add(egui::Label::new(text).sense(egui::Sense::click()))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hover.join("\n"))
}

/// The toolbar's height: its buttons, and the path bar's crumbs in their middle.
const TOOLBAR_ROW: f32 = 26.0;

fn glyph_button(ui: &mut Ui, glyph: Glyph, enabled: bool, hint: &str) -> Response {
    let resp = ui.add_enabled(enabled, egui::Button::new("").min_size(vec2(28.0, TOOLBAR_ROW))).on_hover_text(hint);
    let stroke = ui.visuals().widgets.style(&resp).fg_stroke;
    let stroke = if enabled { stroke } else { Stroke::new(stroke.width, ui.visuals().weak_text_color()) };
    glyphs::paint(ui.painter(), resp.rect, glyph, stroke);
    resp
}

fn glyph_toggle(ui: &mut Ui, glyph: Glyph, on: bool, hint: &str) -> Response {
    let resp = ui.add(egui::Button::new("").selected(on).min_size(vec2(28.0, TOOLBAR_ROW))).on_hover_text(hint);
    let stroke = if on { ui.visuals().selection.stroke } else { ui.visuals().widgets.style(&resp).fg_stroke };
    glyphs::paint(ui.painter(), resp.rect, glyph, stroke);
    resp
}

/// A row in the sidebar: the place's icon and name, filled when it's the
/// tab's location; the name stops `reserve` short of the right, for a button.
fn place_row(ui: &mut Ui, name: &str, icon: Option<egui::TextureId>, current: bool, reserve: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
    let v = ui.visuals();
    let dropping = DragAndDrop::has_payload_of_type::<Dragged>(ui.ctx()) && resp.contains_pointer();
    if current {
        ui.painter().rect_filled(rect, CornerRadius::same(5), v.selection.bg_fill);
    } else if resp.hovered() || dropping {
        ui.painter().rect_filled(rect, CornerRadius::same(5), v.widgets.hovered.weak_bg_fill);
    }
    let colour = if current { v.selection.stroke.color } else { v.text_color() };
    let icon_rect = Rect::from_center_size(
        pos2(rect.left() + 8.0 + PLACE_ICON / 2.0, rect.center().y),
        vec2(PLACE_ICON, PLACE_ICON),
    );
    if let Some(icon) = icon {
        ui.painter().image(icon, icon_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), egui::Color32::WHITE);
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let text = rect.with_max_x(rect.right() - reserve);
    ui.painter_at(text).text(pos2(icon_rect.right() + 8.0, rect.center().y), Align2::LEFT_CENTER, name, font, colour);
    resp
}

/// Takes a drop on `resp` into `location`.
fn drop_target(_ui: &Ui, resp: &Response, location: &Location, a: &mut Vec<Action>) {
    if let Some(dragged) = resp.dnd_release_payload::<Dragged>() {
        a.push(Action::Drop { paths: dragged.0.clone(), into: location.clone() });
    }
}

/// A small arrow in the icon's corner: it's a link to somewhere else.
fn link_badge(painter: &egui::Painter, icon: Rect, v: &egui::Visuals) {
    let size = (icon.width() * 0.35).max(8.0);
    let badge = Rect::from_min_size(icon.left_bottom() - vec2(0.0, size), vec2(size, size));
    painter.rect_filled(badge, CornerRadius::same(2), v.extreme_bg_color);
    let s = Stroke::new(1.2, v.text_color());
    let (a, b) =
        (badge.left_bottom() + vec2(size * 0.25, -size * 0.25), badge.right_top() + vec2(-size * 0.25, size * 0.25));
    painter.line_segment([a, b], s);
    painter.line_segment([b, b - vec2(size * 0.35, 0.0)], s);
    painter.line_segment([b, b + vec2(0.0, size * 0.35)], s);
}

/// Runs `program` with `args` from `dir`, apart from Files, so it outlives it.
fn spawn_in(program: &str, args: &[String], dir: &Path) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;
    let mut child = std::process::Command::new(program)
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crumbs_start_at_home_inside_it() {
        let home = dirs::home_dir().unwrap();
        let names: Vec<String> = crumbs(&Location::Folder(home.join("a/b")), &[]).into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["Home", "a", "b"]);
        let names: Vec<String> = crumbs(&Location::Folder("/usr/share".into()), &[]).into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["Computer", "usr", "share"]);
        assert_eq!(crumbs(&Location::Folder("/usr".into()), &[])[1].1, Location::Folder("/usr".into()));
    }

    #[test]
    fn crumbs_in_a_share_start_at_the_network() {
        let share = PathBuf::from("/run/user/1000/gvfs/smb-share:server=urd.local,share=music");
        let mounts = [network::Mount {
            name: "music on urd.local".into(),
            uri: "smb://urd.local/music/".into(),
            path: share.clone(),
        }];
        let names: Vec<String> =
            crumbs(&Location::Folder(share.join("Albums")), &mounts).into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["Network", "music on urd.local", "Albums"]);
        let names: Vec<String> =
            crumbs(&Location::Network("smb://urd.local/".into()), &mounts).into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["Network", "urd.local"]);
    }

    #[test]
    fn dates_come_out_right() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20_742), (2026, 10, 16));
        assert_eq!(civil(-1), (1969, 12, 31));
    }

    #[test]
    fn typed_locations_expand_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand("~/x"), home.join("x"));
        assert_eq!(expand("~"), home);
        assert_eq!(expand("/etc"), Path::new("/etc"));
        assert_eq!(expand("file:///etc"), Path::new("/etc"));
    }

    #[test]
    fn list_columns_fill_the_width() {
        let wanted = Config::default().list_columns;
        let columns = list_columns(900.0, &wanted);
        assert_eq!(columns.iter().map(|c| c.2).sum::<f32>(), 900.0);
        assert_eq!(list_columns(500.0, &wanted).len(), 3, "no Kind when narrow");
        assert_eq!(list_columns(900.0, &[SortBy::Created]).len(), 2);
    }
}
