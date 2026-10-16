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
use crate::glyphs::{self, Glyph};
use crate::icons::{self, Icons, Source};
use crate::listing::{self, Entry, Kind, Listed, Location, SortBy};
use crate::mime::Mime;
use crate::ops::{self, Op};
use crate::places::{self, Place};
use crate::uri;
use crate::watch::Watcher;

/// Files' skins folder is `~/.config/files/skins`.
const APP: &str = "files";

/// The icon view's icons, and each item's cell, in points.
const ICON: f32 = 48.0;
const CELL: egui::Vec2 = vec2(104.0, 104.0);
/// The list view's rows and their icons.
const ROW: f32 = 24.0;
const ROW_ICON: f32 = 16.0;
/// The sidebar's icons.
const PLACE_ICON: f32 = 16.0;
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
            Location::Trash => None,
        }
    }

    fn in_trash(&self) -> bool {
        self.location == Location::Trash
    }

    /// The entries shown: hidden ones only when asked, and only those the
    /// filter matches, as indices into `entries`.
    fn visible(&self, show_hidden: bool) -> Vec<usize> {
        let filter = self.filter.trim().to_lowercase();
        (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                let typed =
                    e.kind != Kind::File || self.types.as_ref().is_none_or(|t| t.matches(&e.name, e.mime.as_deref()));
                typed && (show_hidden || !e.hidden) && (filter.is_empty() || e.name.to_lowercase().contains(&filter))
            })
            .collect()
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

enum Confirm {
    Purge(Vec<trash::TrashItem>),
    EmptyTrash,
    /// Saving over a file that's there.
    Replace(PathBuf),
}

/// How the window starts: browsing, or choosing for an app (ADR-0004).
pub enum Start {
    Browse(Option<PathBuf>),
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
    NewWindow,
    Drop {
        paths: Vec<PathBuf>,
        into: Location,
    },
    Move(isize),
    Tab(TabAction),
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
}

pub struct Files {
    config: Config,
    skins: Vec<Skin>,
    skin: usize,
    skin_errors: Vec<String>,
    tabs: Vec<Tab>,
    tab: usize,
    mime: Arc<Mime>,
    icons: Icons,
    places: Vec<Place>,
    disks: Vec<Place>,
    /// Folders with their own icon: home, Apps, Documents and the rest.
    special: HashMap<PathBuf, Source>,
    watcher: Watcher,
    listed_send: Sender<Listed>,
    listed: Receiver<Listed>,
    /// Listings being read, and ones that changed again while they were.
    reading: HashSet<Location>,
    stale: HashSet<Location>,
    worker: ops::Worker,
    clip: Option<Clip>,
    renaming: Option<Renaming>,
    location_edit: Option<String>,
    /// Whether the location field has been focused, with its text selected.
    location_started: bool,
    focus_filter: bool,
    confirm: Option<Confirm>,
    /// Something to tell the user, until they do something else.
    notice: Option<String>,
    typed: (String, Instant),
    /// The icon view's columns, for the arrow keys.
    columns: usize,
    /// Where the item area's content starts on screen, scrolled.
    origin: egui::Pos2,
    /// A drag on the background: where it started, in content coordinates,
    /// and what was selected before it.
    band: Option<(egui::Pos2, HashSet<PathBuf>)>,
    /// The item area's scroll offset and height, last frame.
    viewport: (f32, f32),
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

        let (start, picking) = match start {
            Start::Browse(path) => (path, None),
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
                Location::Trash => None,
            })
            .collect();
        special.remove(Path::new("/"));

        let mut files = Files {
            config,
            skins,
            skin,
            skin_errors,
            tabs: vec![first],
            tab: 0,
            mime: Arc::new(Mime::load()),
            icons: Icons::new(ctx),
            places,
            disks: places::disks(),
            special,
            watcher: Watcher::new(ctx),
            listed_send,
            listed,
            reading: HashSet::new(),
            stale: HashSet::new(),
            worker: ops::Worker::new(ctx),
            clip: None,
            renaming: None,
            location_edit: None,
            location_started: false,
            focus_filter: false,
            confirm: None,
            notice: None,
            typed: (String::new(), Instant::now()),
            columns: 1,
            origin: egui::Pos2::ZERO,
            band: None,
            viewport: (0.0, 0.0),
            actions: Vec::new(),
            title: String::new(),
            screenshot: std::env::var_os("FILES_SCREENSHOT").map(|p| (PathBuf::from(p), false)),
            picking,
        };
        files.apply_skin(ctx);
        if files.config.system_title_bar {
            ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(true));
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
        listing::spawn(location, self.mime.clone(), ctx.clone(), self.listed_send.clone());
    }

    /// Takes in finished listings, file changes and finished operations.
    fn poll(&mut self, ctx: &egui::Context) {
        self.icons.poll(ctx);
        while let Ok(Listed { location, entries }) = self.listed.try_recv() {
            self.reading.remove(&location);
            if self.stale.remove(&location) {
                self.load(location.clone(), ctx);
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
                let wanted: Vec<PathBuf> = tab.select_after.iter().filter(|p| present.contains(*p)).cloned().collect();
                if !wanted.is_empty() {
                    tab.select_only(wanted);
                    tab.select_after.clear();
                }
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

        for changed in self.watcher.changed() {
            if changed == listing::trash_files() || changed.ends_with("Trash/info") {
                self.load(Location::Trash, ctx);
            } else if places::mount_dirs().contains(&changed) {
                self.disks = places::disks();
            } else {
                self.load(Location::Folder(changed), ctx);
            }
        }

        let placed = {
            let mut status = self.worker.status.lock().unwrap_or_else(|e| e.into_inner());
            if status.running.is_none() { std::mem::take(&mut status.placed) } else { Vec::new() }
        };
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
        folders.extend(places::mount_dirs());
        self.watcher.set(folders);
    }

    fn icon_source(&self, entry: &Entry) -> Source {
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

    fn title_of(location: &Location) -> String {
        match location {
            Location::Trash => "Trash".into(),
            Location::Folder(path) if Some(path) == dirs::home_dir().as_ref() => "Home".into(),
            Location::Folder(path) if path == Path::new("/") => "Computer".into(),
            Location::Folder(path) => places::name(path),
        }
    }

    // ---- Doing what was asked ----

    fn apply(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::Open(path) => self.open(&path, ctx),
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
                if let Some(folder) = self.current().folder().map(Path::to_path_buf)
                    && let Some(parent) = folder.parent()
                {
                    self.go(Location::Folder(parent.to_path_buf()), ctx);
                    self.current_mut().select_after = vec![folder];
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
                if !paths.is_empty() && !self.current().in_trash() {
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
                } else if !paths.is_empty() {
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
                        Err(e) => self.notice = Some(format!("Can't make a folder here: {e}")),
                    }
                }
            }
            Action::Terminal(dir) => {
                if let Err(e) = spawn_in(&patina_apps::terminal(), &[], &dir) {
                    self.notice = Some(format!("Can't open a terminal: {e}"));
                }
            }
            Action::NewWindow => {
                let here = self.current().folder().map(Path::to_path_buf);
                let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("files"));
                let args: Vec<String> = here.iter().map(|p| p.display().to_string()).collect();
                if let Err(e) = spawn_in(&exe.to_string_lossy(), &args, &dirs::home_dir().unwrap_or_default()) {
                    self.notice = Some(format!("Can't open a window: {e}"));
                }
            }
            Action::Drop { paths, into } => {
                let copy = ctx.input(|i| i.modifiers.ctrl);
                self.drop_into(paths, into, copy);
            }
            Action::Move(step) => self.step(step, ctx.input(|i| i.modifiers.shift)),
            Action::Tab(action) => self.tab_action(action, ctx),
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
                self.location_edit = Some(match &self.current().location {
                    Location::Folder(path) => path.display().to_string(),
                    Location::Trash => String::new(),
                });
            }
            Action::FocusFilter => self.focus_filter = true,
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
        self.notice = None;
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
        self.load(location, ctx);
    }

    /// Opens what's at `path`: goes into a folder, runs an app, and hands a
    /// file to the desktop's app for it.
    fn open(&mut self, path: &Path, ctx: &egui::Context) {
        let Some(entry) = self.current().entries.iter().find(|e| e.path == path).cloned() else {
            return;
        };
        if entry.trashed.is_some() {
            self.notice = Some(format!("Put “{}” back to open it.", entry.name));
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
                Err(e) => self.notice = Some(format!("Can't open {}: {e}", entry.name)),
            },
            Kind::File => {
                if let Err(e) = patina::open::path(&entry.path) {
                    self.notice = Some(format!("Can't open {}: {e}", entry.name));
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
        if self.current().in_trash() {
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
            Err(e) => self.notice = Some(e),
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
        if ctx.text_edit_focused() || self.renaming.is_some() || self.confirm.is_some() {
            return;
        }
        let mut actions = Vec::new();
        let mut typed = String::new();
        let columns = if self.config.view == View::Icons { self.columns as isize } else { 1 };
        let page = if self.config.view == View::Icons { columns * 4 } else { 12 };
        let in_trash = self.current().in_trash();
        let picking = self.picking.is_some();
        ctx.input_mut(|i| {
            i.events.retain(|event| {
                let action = match event {
                    Event::Copy => Action::Copy,
                    Event::Cut => Action::Cut,
                    Event::Paste(text) => Action::Paste(Some(text.clone())),
                    Event::Text(text) if !text.chars().any(char::is_control) => {
                        typed.push_str(text);
                        return false;
                    }
                    Event::Key { key, pressed: true, modifiers: m, .. } => {
                        let plain = !m.command && !m.alt;
                        match key {
                            Key::Enter if plain && picking => Action::Accept,
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
                            Key::T if m.command && !m.shift => Action::Tab(TabAction::New),
                            Key::W if m.command => Action::CloseTab,
                            Key::Q if m.command => Action::Quit,
                            Key::H if m.command => Action::ToggleHidden,
                            Key::L if m.command => Action::EditLocation,
                            Key::F if m.command => Action::FocusFilter,
                            Key::Num1 if m.command => Action::SetView(View::Icons),
                            Key::Num2 if m.command => Action::SetView(View::List),
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
        let titles: Vec<String> = self.tabs.iter().map(|t| Self::title_of(&t.location)).collect();
        let mut action = None;
        TitleBar::new("", style).custom(!self.config.system_title_bar).menu_bar(true).show(
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
        if ui.add(egui::Button::new("Close Tab").shortcut_text(shortcut(ui, "Ctrl+W"))).clicked() {
            a.push(Action::CloseTab);
        }
        self.actions.extend(a);
    }

    fn edit_menu(&mut self, ui: &mut Ui) {
        let tab = self.current();
        let in_trash = tab.in_trash();
        let count = tab.selected.len();
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
            if ui.add_enabled(self.clip.is_some(), paste).clicked() {
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
        let up = tab.folder().is_some_and(|f| f.parent().is_some());
        if ui.add_enabled(up, egui::Button::new("Enclosing Folder").shortcut_text(shortcut(ui, "Alt+Up"))).clicked() {
            a.push(Action::Up);
        }
        ui.separator();
        for place in self.places.iter().chain(&self.disks) {
            if ui.button(&place.name).clicked() {
                a.push(Action::Go(place.location.clone()));
            }
        }
        ui.separator();
        if ui.add(egui::Button::new("Location…").shortcut_text(shortcut(ui, "Ctrl+L"))).clicked() {
            a.push(Action::EditLocation);
        }
        self.actions.extend(a);
    }

    fn skin_menu(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let mut changed = false;
        for (i, skin) in self.skins.iter().enumerate() {
            let resp = ui.radio_value(&mut self.skin, i, &skin.name);
            let resp = match (&skin.path, &skin.author) {
                (Some(path), _) => resp.on_hover_text(path.display().to_string()),
                (None, Some(author)) => resp.on_hover_text(format!("Built in, by {author}")),
                (None, None) => resp.on_hover_text("Stock egui"),
            };
            changed |= resp.clicked();
        }
        ui.separator();
        ui.add_enabled_ui(self.skins[self.skin].follows_desktop(), |ui| {
            for (value, label) in [("system", "Follow Desktop"), ("light", "Light"), ("dark", "Dark")] {
                if ui.radio(self.config.light_dark == value, label).clicked() {
                    self.config.light_dark = value.into();
                    changed = true;
                }
            }
        });
        ui.separator();
        if ui.button("Reload Skins").clicked() {
            let name = self.skins[self.skin].name.clone();
            (self.skins, self.skin_errors) = load_skins();
            self.skin = self.skins.iter().position(|s| s.name == name).unwrap_or(0);
            changed = true;
        }
        if let Some(dir) = skin::app_dir(APP)
            && ui.button("Open Skins Folder").on_hover_text(dir.display().to_string()).clicked()
        {
            let _ = std::fs::create_dir_all(&dir);
            self.actions.push(Action::OpenInNewTab(Location::Folder(dir)));
        }
        for error in &self.skin_errors {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if changed {
            self.config.skin = self.skins[self.skin].name.clone();
            self.config.save();
            self.apply_skin(&ctx);
        }
    }

    /// Back, forward and up; the path, or a field to type one; the view's
    /// buttons and the filter.
    fn toolbar(&mut self, ui: &mut Ui) {
        let tab = self.current();
        let (can_back, can_forward) = (!tab.back.is_empty(), !tab.forward.is_empty());
        let can_up = tab.folder().is_some_and(|f| f.parent().is_some());
        let in_trash = tab.in_trash();
        let has_entries = !tab.entries.is_empty();
        let mut a = Vec::new();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            if glyph_button(ui, Glyph::Back, can_back, "Back (Alt+Left)").clicked() {
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
    /// or a double click on the bar turns it into a field.
    fn path_bar(&mut self, ui: &mut Ui, a: &mut Vec<Action>) {
        if let Some(text) = &mut self.location_edit {
            let all = text.chars().count();
            let resp = ui.add(egui::TextEdit::singleline(text).desired_width(ui.available_width()));
            if !self.location_started {
                // All of it selected, so typing replaces it.
                self.location_started = true;
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), resp.id) {
                    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(all))));
                    state.store(ui.ctx(), resp.id);
                }
                resp.request_focus();
            }
            if resp.lost_focus() {
                let text = self.location_edit.take().unwrap_or_default();
                if ui.input(|i| i.key_pressed(Key::Enter)) {
                    let path = expand(&text);
                    if path.is_dir() {
                        a.push(Action::Go(Location::Folder(path)));
                    } else if self.picking.is_some() && (path.is_file() || path.parent().is_some_and(Path::is_dir)) {
                        a.push(Action::ChoosePath(path));
                    } else if let Some(parent) = path.parent().filter(|_| path.exists()) {
                        a.push(Action::Go(Location::Folder(parent.to_path_buf())));
                        self.tabs[self.tab].select_after = vec![path];
                    } else {
                        self.notice = Some(format!("There's no folder at {}", path.display()));
                    }
                }
            }
            return;
        }
        let crumbs = crumbs(&self.current().location);
        let bar = ui.available_rect_before_wrap();
        let background = ui.interact(bar, ui.id().with("path-bar"), Sense::click());
        if background.double_clicked() {
            a.push(Action::EditLocation);
        }
        egui::ScrollArea::horizontal()
            .stick_to_right(true)
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
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
                        let resp = ui.add(egui::Button::new(text).frame(false).min_size(vec2(0.0, 24.0)));
                        if i != last && resp.clicked() {
                            a.push(Action::Go(location.clone()));
                        }
                        drop_target(ui, &resp, &location, a);
                    }
                });
            });
    }

    fn sidebar(&mut self, ui: &mut Ui) {
        let here = self.current().location.clone();
        let px = (PLACE_ICON * ui.ctx().pixels_per_point()).round() as u32;
        let mut a = Vec::new();
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(6.0);
            for (heading, list) in [("Places", &self.places), ("Disks", &self.disks)] {
                ui.label(egui::RichText::new(heading).small().weak());
                for place in list {
                    let icon = self.icons.get(&place.icon, px);
                    let resp = place_row(ui, &place.name, icon, place.location == here);
                    if resp.clicked() {
                        a.push(Action::Go(place.location.clone()));
                    }
                    if resp.clicked_by(egui::PointerButton::Middle) {
                        a.push(Action::OpenInNewTab(place.location.clone()));
                    }
                    drop_target(ui, &resp, &place.location, &mut a);
                }
                ui.add_space(10.0);
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
                if let Some(notice) = &self.notice {
                    let text = egui::RichText::new(notice).color(ui.visuals().error_fg_color);
                    ui.add(egui::Label::new(text).truncate());
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

    fn status_bar(&mut self, ui: &mut Ui) {
        let status = self.worker.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let tab = self.current();
        let visible = tab.visible(self.config.show_hidden).len();
        let selection = tab.selection();
        let items = |n: usize| if n == 1 { "1 item".to_string() } else { format!("{n} items") };
        let text = if selection.is_empty() {
            items(visible)
        } else {
            let bytes: u64 = selection.iter().filter(|e| e.kind == Kind::File).map(|e| e.size).sum();
            let size = if bytes > 0 { format!(", {}", listing::human_size(bytes)) } else { String::new() };
            format!("{} of {} selected{size}", selection.len(), visible)
        };
        ui.horizontal(|ui| {
            ui.label(text);
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if let Some((label, progress)) = &status.running {
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
                } else if let Some(message) = self.notice.clone().or_else(|| status.errors.first().cloned()) {
                    if ui.small_button("Dismiss").clicked() {
                        self.notice = None;
                        self.worker.status.lock().unwrap_or_else(|e| e.into_inner()).errors.clear();
                    }
                    let more = status.errors.len().saturating_sub(1);
                    let message = if more > 0 && self.notice.is_none() {
                        format!("{message} (and {more} more)")
                    } else {
                        message
                    };
                    let resp = ui.colored_label(ui.visuals().error_fg_color, message);
                    if status.errors.len() > 1 {
                        resp.on_hover_text(status.errors.join("\n"));
                    }
                }
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

        if visible.is_empty() && tab.loaded {
            let empty = if tab.in_trash() {
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
            View::List => self.list_view(ui, &visible),
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
        background.context_menu(|ui| {
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
            View::List => Rect::from_min_size(pos2(0.0, n as f32 * ROW), vec2(width, ROW)),
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
        self.item_input(ui, &resp, &entry, selected);
    }

    fn list_view(&mut self, ui: &mut Ui, visible: &[usize]) {
        let width = ui.available_width();
        let columns = list_columns(width);
        self.columns = 1;
        // The header: click a column to sort by it, again to reverse.
        let (header, _) = ui.allocate_exact_size(vec2(width, ROW), Sense::hover());
        let mut x = header.left();
        for (by, label, w, right) in &columns {
            let rect = Rect::from_min_size(pos2(x, header.top()), vec2(*w, ROW));
            let resp = ui.interact(rect, Id::new(("column", label)), Sense::click());
            let mut text = label.to_string();
            if self.config.sort.by == *by {
                text.push_str(if self.config.sort.descending { " ▼" } else { " ▲" });
            }
            let (align, at) = if *right {
                (Align2::RIGHT_CENTER, rect.right_center() - vec2(8.0, 0.0))
            } else {
                (Align2::LEFT_CENTER, rect.left_center() + vec2(8.0, 0.0))
            };
            let colour = if resp.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
            ui.painter().text(at, align, text, egui::TextStyle::Small.resolve(ui.style()), colour);
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
                    Kind::Folder if entry.trashed.is_none() => {
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
        self.item_input(ui, &resp, &entry, selected);
    }

    /// Clicks, drags, drops and the menu on one item.
    fn item_input(&mut self, ui: &Ui, resp: &Response, entry: &Entry, selected: bool) {
        let m = ui.input(|i| i.modifiers);
        if resp.clicked() {
            self.actions.push(Action::Click { path: entry.path.clone(), add: m.command, range: m.shift });
        }
        if resp.double_clicked() {
            self.actions.push(Action::Open(entry.path.clone()));
        }
        if resp.clicked_by(egui::PointerButton::Middle) && entry.is_folder() {
            self.actions.push(Action::OpenInNewTab(Location::Folder(entry.path.clone())));
        }
        if resp.secondary_clicked() && !selected {
            self.actions.push(Action::SelectOnly(vec![entry.path.clone()]));
        }
        let tab = self.current();
        let in_trash = tab.in_trash();
        let count = if selected { tab.selected.len() } else { 1 };
        let selection: Vec<PathBuf> = tab.selection().iter().map(|e| e.path.clone()).collect();
        if resp.drag_started() && !in_trash {
            let paths = if selected {
                selection
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
        resp.context_menu(|ui| {
            if in_trash {
                if ui.button("Put Back").clicked() {
                    a.push(Action::Restore);
                }
                if ui.button("Delete Permanently…").clicked() {
                    a.push(Action::Purge);
                }
                return;
            }
            if ui.button("Open").clicked() {
                a.push(Action::Open(entry.path.clone()));
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

    /// What's being dragged, drawn beside the pointer.
    fn drag_ghost(&self, ctx: &egui::Context) {
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

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match &self.picking {
            Some(picking) => picking.request.title(),
            None => Self::title_of(&self.current().location),
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
            self.actions.push(Action::Tab(action));
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
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).inner_margin(egui::Margin::same(4)))
            .show(ui, |ui| self.items(ui));

        self.drag_ghost(&ctx);
        self.dialogs(&ctx);
        for action in std::mem::take(&mut self.actions) {
            self.apply(action, &ctx);
        }
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
        patina::ime::drop_repeated_preedits(ctx, raw_input);
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }
}

fn load_skins() -> (Vec<Skin>, Vec<String>) {
    skin::load_all(skin::BUILT_IN, &skin::app_dir(APP).into_iter().collect::<Vec<_>>())
}

/// The list's columns: what each sorts by, its heading, width and whether
/// it's right-aligned. Name takes what the others leave; Kind goes first
/// when the window is narrow.
fn list_columns(width: f32) -> Vec<(SortBy, &'static str, f32, bool)> {
    let mut rest = vec![(SortBy::Size, "Size", 90.0, true), (SortBy::Modified, "Modified", 150.0, false)];
    if width > 620.0 {
        rest.push((SortBy::Kind, "Kind", 200.0, false));
    }
    let used: f32 = rest.iter().map(|c| c.2).sum();
    let mut columns = vec![(SortBy::Name, "Name", (width - used).max(120.0), false)];
    columns.extend(rest);
    columns
}

/// The path as places to go: from Home when it's inside it, else from the
/// computer's root.
fn crumbs(location: &Location) -> Vec<(String, Location)> {
    let Location::Folder(path) = location else {
        return vec![("Trash".into(), Location::Trash)];
    };
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

fn glyph_button(ui: &mut Ui, glyph: Glyph, enabled: bool, hint: &str) -> Response {
    let resp = ui.add_enabled(enabled, egui::Button::new("").min_size(vec2(28.0, 26.0))).on_hover_text(hint);
    let stroke = ui.visuals().widgets.style(&resp).fg_stroke;
    let stroke = if enabled { stroke } else { Stroke::new(stroke.width, ui.visuals().weak_text_color()) };
    glyphs::paint(ui.painter(), resp.rect, glyph, stroke);
    resp
}

fn glyph_toggle(ui: &mut Ui, glyph: Glyph, on: bool, hint: &str) -> Response {
    let resp = ui.add(egui::Button::new("").selected(on).min_size(vec2(28.0, 26.0))).on_hover_text(hint);
    let stroke = if on { ui.visuals().selection.stroke } else { ui.visuals().widgets.style(&resp).fg_stroke };
    glyphs::paint(ui.painter(), resp.rect, glyph, stroke);
    resp
}

/// A row in the sidebar: the place's icon and name, filled when it's the
/// tab's location.
fn place_row(ui: &mut Ui, name: &str, icon: Option<egui::TextureId>, current: bool) -> Response {
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
    ui.painter_at(rect).text(pos2(icon_rect.right() + 8.0, rect.center().y), Align2::LEFT_CENTER, name, font, colour);
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
        let names: Vec<String> = crumbs(&Location::Folder(home.join("a/b"))).into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["Home", "a", "b"]);
        let names: Vec<String> = crumbs(&Location::Folder("/usr/share".into())).into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["Computer", "usr", "share"]);
        assert_eq!(crumbs(&Location::Folder("/usr".into()))[1].1, Location::Folder("/usr".into()));
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
        let columns = list_columns(900.0);
        assert_eq!(columns.iter().map(|c| c.2).sum::<f32>(), 900.0);
        assert_eq!(list_columns(500.0).len(), 3, "no Kind when narrow");
    }
}
