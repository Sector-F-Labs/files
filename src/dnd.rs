//! Drag and drop with other apps, and other Files windows, on Wayland.
//!
//! Out (ADR-0006): a drag of files is a Wayland drag from the start,
//! offering them as `text/uri-list` with their icon under the pointer, so
//! any window can take them.
//!
//! In (ADR-0007): a drag of files from another app is taken over Files'
//! window, its list read as soon as it comes over, so egui has the paths to
//! drag before it's let go.
//!
//! Either way, while a drag is over Files' window its events come back as
//! `Over`s, which the app gives egui as pointer events, so its drops (into
//! folders, onto places) are egui's own, as inside one window.
//!
//! winit can't start a Wayland drag, so this opens a connection of its own
//! onto winit's display, as smithay-clipboard does for the clipboard: its
//! own seat and pointer (for the serial of the press the drag continues),
//! data device and event queue, dispatched on a thread so the files are
//! sent when the other app asks, however idle the window is.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use egui::ColorImage;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use wayland_backend::client::{Backend, ObjectId};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::{
    wl_buffer::{self, WlBuffer},
    wl_compositor::WlCompositor,
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::WlDataDeviceManager,
    wl_data_offer::WlDataOffer,
    wl_data_source::{self, WlDataSource},
    wl_pointer::{self, WlPointer},
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_shm::{self, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child};

const URI_LIST: &str = "text/uri-list";
/// The icon under the pointer, in points, and where the pointer is on it.
pub const ICON: u32 = 64;
const HOTSPOT: i32 = 20;

/// How long a drop waits for the other app to say which files it holds.
const READ_WAIT: Duration = Duration::from_secs(2);

/// A drag over our window, in its points.
#[derive(Debug, Clone, PartialEq)]
pub enum Over {
    /// Another app's drag has come over, holding these files.
    Incoming(Vec<PathBuf>),
    Moved(egui::Pos2),
    Left,
    /// Let go here; `copy` if the drop is a copy (Ctrl), not a move.
    Dropped {
        at: egui::Pos2,
        copy: bool,
    },
    /// Our own drag is over, wherever it was dropped, or it was called off.
    Ended,
}

/// What an offer holds and the action chosen for it, as the compositor
/// tells us.
#[derive(Default)]
struct OfferInfo {
    types: Mutex<Vec<String>>,
    action: Mutex<Option<DndAction>>,
}

/// A drag over our window: its offer, whether it's ours, and for another
/// app's, word when its files have been read.
struct Hover {
    offer: WlDataOffer,
    own: bool,
    read: Option<Receiver<()>>,
}

/// What the thread and the window share.
#[derive(Default)]
struct Shared {
    device: Option<WlDataDevice>,
    /// The serial of the press the pointer is held down from, if it is.
    press: Option<u32>,
    /// A drag we started is under way.
    dragging: bool,
    /// Its icon's surface, let go when it ends.
    icon: Option<WlSurface>,
}

pub struct Dnd {
    conn: Connection,
    qh: QueueHandle<State>,
    shared: Arc<Mutex<Shared>>,
    /// winit's window surface, where the drag starts.
    window: WlSurface,
    manager: WlDataDeviceManager,
    compositor: WlCompositor,
    shm: WlShm,
    over: Receiver<Over>,
}

impl Dnd {
    /// Onto the window's Wayland connection; `None` elsewhere (X11), or if
    /// the compositor lacks something. `ctx` is woken for each `Over`.
    pub fn new(ctx: &egui::Context, display: RawDisplayHandle, window: RawWindowHandle) -> Option<Self> {
        let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(window)) = (display, window) else {
            return None;
        };
        // SAFETY: winit's display lives as long as the window, which
        // outlives the app and this.
        let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, mut queue): (_, EventQueue<State>) = registry_queue_init(&conn).ok()?;
        let qh = queue.handle();
        let manager: WlDataDeviceManager = globals.bind(&qh, 3..=3, ()).ok()?;
        // Version 4, where the icon's position comes with attach.
        let compositor: WlCompositor = globals.bind(&qh, 4..=4, ()).ok()?;
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).ok()?;
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).ok()?;
        // SAFETY: the pointer is winit's live wl_surface proxy.
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), window.surface.as_ptr().cast()) }.ok()?;
        let window = WlSurface::from_id(&conn, id).ok()?;
        let shared = Arc::new(Mutex::new(Shared {
            device: Some(manager.get_data_device(&seat, &qh, ())),
            ..Default::default()
        }));
        let (send, over) = channel();
        let mut state = State {
            shared: shared.clone(),
            pointer: None,
            window: window.clone(),
            hover: None,
            at: egui::Pos2::ZERO,
            over: send,
            ctx: ctx.clone(),
        };
        queue.roundtrip(&mut state).ok()?;
        std::thread::Builder::new()
            .name("drag-out".into())
            .spawn(move || while queue.blocking_dispatch(&mut state).is_ok() {})
            .ok()?;
        Some(Self { conn, qh, shared, window, manager, compositor, shm, over })
    }

    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What drags did over our window since last asked.
    pub fn over(&self) -> Vec<Over> {
        self.over.try_iter().collect()
    }

    /// Hands `paths` to the compositor as a drag, continuing the press
    /// that's held. The icon is drawn by `icon` on a thread and shown when
    /// it's ready, `scale` pixels to the point. False if there's no press
    /// to continue.
    pub fn start(
        &self,
        paths: &[PathBuf],
        scale: f32,
        icon: impl FnOnce(u32) -> Option<ColorImage> + Send + 'static,
    ) -> bool {
        let mut shared = self.shared();
        let (Some(serial), Some(device)) = (shared.press, shared.device.clone()) else {
            return false;
        };
        let list: Arc<[u8]> = crate::uri::drag_list(paths).into_bytes().into();
        let source = self.manager.create_data_source(&self.qh, list);
        source.offer(URI_LIST.into());
        // Move too, for other Files windows, which move the files
        // themselves; a drag out never deletes anything here.
        source.set_actions(DndAction::Copy | DndAction::Move);
        let surface = self.compositor.create_surface(&self.qh, ());
        device.start_drag(Some(&source), &self.window, Some(&surface), serial);
        shared.dragging = true;
        if let Some(old) = shared.icon.replace(surface.clone()) {
            old.destroy();
        }
        drop(shared);
        let _ = self.conn.flush();

        let (conn, qh, shm) = (self.conn.clone(), self.qh.clone(), self.shm.clone());
        let scale = scale.ceil().max(1.0) as i32;
        let _ = std::thread::Builder::new().name("drag-icon".into()).spawn(move || {
            if let Some(image) = icon(ICON * scale as u32) {
                show_icon(&surface, &shm, &qh, &image, scale);
                let _ = conn.flush();
            }
        });
        true
    }
}

/// Puts `image` on the drag icon's surface, at `scale`, with the pointer
/// at the hotspot.
fn show_icon(surface: &WlSurface, shm: &WlShm, qh: &QueueHandle<State>, image: &ColorImage, scale: i32) {
    let [w, h] = image.size;
    let stride = w * 4;
    // ARGB8888 is BGRA in memory; egui's colours are premultiplied, as
    // wl_shm wants.
    let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| [c.b(), c.g(), c.r(), c.a()]).collect();
    let Some(fd) = shared_memory(&bytes) else { return };
    let pool = shm.create_pool(fd.as_fd(), bytes.len() as i32, qh, ());
    let buffer = pool.create_buffer(0, w as i32, h as i32, stride as i32, wl_shm::Format::Argb8888, qh, ());
    pool.destroy();
    surface.set_buffer_scale(scale);
    surface.attach(Some(&buffer), -HOTSPOT, -HOTSPOT);
    surface.damage_buffer(0, 0, w as i32, h as i32);
    surface.commit();
}

/// A memory file holding `bytes`, to share with the compositor.
fn shared_memory(bytes: &[u8]) -> Option<OwnedFd> {
    use std::io::Write;
    // SAFETY: a fresh descriptor, owned from here.
    let fd = unsafe { libc::memfd_create(c"files-drag-icon".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes).ok()?;
    Some(file.into())
}

/// The files in a `text/uri-list`, leaving out what isn't a local file.
fn paths(list: &str) -> Vec<PathBuf> {
    list.lines().filter(|l| !l.starts_with('#')).filter_map(crate::uri::to_path).collect()
}

/// Asks for `offer`'s list and reads it on a thread, telling the window
/// the files, then `done`.
fn read_list(offer: &WlDataOffer, conn: &Connection, over: Sender<Over>, ctx: egui::Context) -> Option<Receiver<()>> {
    let (mut reader, writer) = std::io::pipe().ok()?;
    offer.receive(URI_LIST.into(), writer.as_fd());
    drop(writer);
    let _ = conn.flush();
    let (done, read) = channel();
    std::thread::Builder::new()
        .name("drag-read".into())
        .spawn(move || {
            use std::io::Read;
            let mut list = String::new();
            let _ = reader.read_to_string(&mut list);
            let _ = over.send(Over::Incoming(paths(&list)));
            ctx.request_repaint();
            let _ = done.send(());
        })
        .ok()?;
    Some(read)
}

/// The thread's side: the pointer's presses, the drag's data source, and
/// drags over our window.
struct State {
    shared: Arc<Mutex<Shared>>,
    pointer: Option<WlPointer>,
    window: WlSurface,
    /// The drag over our window, if one is.
    hover: Option<Hover>,
    /// Where it is there.
    at: egui::Pos2,
    over: Sender<Over>,
    ctx: egui::Context,
}

impl State {
    fn shared(&self) -> std::sync::MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tell(&self, over: Over) {
        let _ = self.over.send(over);
        self.ctx.request_repaint();
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            let has = caps.contains(wl_seat::Capability::Pointer);
            match (has, &state.pointer) {
                (true, None) => state.pointer = Some(seat.get_pointer(qh, ())),
                (false, Some(p)) => {
                    if p.version() >= 3 {
                        p.release();
                    }
                    state.pointer = None;
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Button { serial, state: WEnum::Value(wl_pointer::ButtonState::Pressed), .. } => {
                state.shared().press = Some(serial);
            }
            wl_pointer::Event::Button { state: WEnum::Value(wl_pointer::ButtonState::Released), .. }
            | wl_pointer::Event::Leave { .. } => state.shared().press = None,
            _ => {}
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        conn: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Drags of files are taken over our window, ours or another app's;
        // anything else, and the clipboard, is let go at once.
        match event {
            wl_data_device::Event::Enter { serial, surface, x, y, id: Some(offer) } => {
                let own = state.shared().dragging;
                let files = offer
                    .data::<OfferInfo>()
                    .is_some_and(|i| i.types.lock().is_ok_and(|t| t.iter().any(|t| t == URI_LIST)));
                if surface != state.window || !(own || files) {
                    offer.destroy();
                    return;
                }
                offer.accept(serial, Some(URI_LIST.into()));
                offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Move);
                let read = if own { None } else { read_list(&offer, conn, state.over.clone(), state.ctx.clone()) };
                state.hover = Some(Hover { offer, own, read });
                state.at = egui::pos2(x as f32, y as f32);
                state.tell(Over::Moved(state.at));
            }
            wl_data_device::Event::Motion { x, y, .. } if state.hover.is_some() => {
                state.at = egui::pos2(x as f32, y as f32);
                state.tell(Over::Moved(state.at));
            }
            wl_data_device::Event::Leave => {
                if let Some(hover) = state.hover.take() {
                    hover.offer.destroy();
                    state.tell(Over::Left);
                }
            }
            wl_data_device::Event::Drop => {
                if let Some(hover) = state.hover.take() {
                    // The files must be known before egui is let go.
                    if let Some(read) = &hover.read {
                        let _ = read.recv_timeout(READ_WAIT);
                    }
                    let action = hover.offer.data::<OfferInfo>().and_then(|i| *i.action.lock().ok()?);
                    if action.is_some() {
                        hover.offer.finish();
                    }
                    hover.offer.destroy();
                    let copy = action == Some(DndAction::Copy) && !hover.own;
                    state.tell(Over::Dropped { at: state.at, copy });
                }
            }
            wl_data_device::Event::Selection { id: Some(offer) } => offer.destroy(),
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferInfo::default()),
    ]);
}

impl Dispatch<WlDataOffer, OfferInfo> for State {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: <WlDataOffer as Proxy>::Event,
        info: &OfferInfo,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wayland_client::protocol::wl_data_offer::Event;
        match event {
            Event::Offer { mime_type } => {
                if let Ok(mut types) = info.types.lock() {
                    types.push(mime_type);
                }
            }
            Event::Action { dnd_action: WEnum::Value(action) } => {
                if let Ok(mut chosen) = info.action.lock() {
                    *chosen = Some(action);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, Arc<[u8]>> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        list: &Arc<[u8]>,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } if mime_type == URI_LIST => {
                // On a thread: the other app reads at its own pace.
                let list = list.clone();
                let _ = std::thread::Builder::new().name("drag-send".into()).spawn(move || {
                    use std::io::Write;
                    let _ = std::fs::File::from(fd).write_all(&list);
                });
            }
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                source.destroy();
                let mut shared = state.shared();
                shared.dragging = false;
                if let Some(icon) = shared.icon.take() {
                    icon.destroy();
                }
                drop(shared);
                state.tell(Over::Ended);
            }
            _ => {}
        }
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(_: &mut Self, buffer: &WlBuffer, event: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            buffer.destroy();
        }
    }
}

macro_rules! no_events {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
no_events!(WlDataDeviceManager, WlCompositor, WlShm, WlShmPool, WlSurface);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_gives_its_local_files() {
        let list = "# from Nautilus\r\nfile:///home/a/b%20c.png\r\nhttps://example.com/x.png\r\nfile:///tmp/y\r\n";
        assert_eq!(paths(list), [PathBuf::from("/home/a/b c.png"), PathBuf::from("/tmp/y")]);
    }
}
