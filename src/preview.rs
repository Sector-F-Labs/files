//! Quick Look's pictures (ADR-0008): a picture decoded at about the
//! screen's size on a thread, turned the way its camera said. Only the newest
//! request is decoded, so stepping through a folder doesn't queue up every
//! picture passed; the last few stay as textures, to step back to.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use egui::{ColorImage, TextureHandle, TextureOptions};
use image::{DynamicImage, ImageDecoder};

/// Pictures larger than this aren't decoded.
const LIMIT: u64 = 256_000_000;
/// How many previews stay ready.
const KEPT: usize = 8;

/// A picture, by its path, the time it was changed and the size asked for.
type Key = (PathBuf, Option<SystemTime>, u32);

/// A picture ready to show.
pub struct Picture {
    pub texture: TextureHandle,
    /// Its pixels as decoded, which are fewer than the file's when it's
    /// larger than the screen.
    pub drawn: egui::Vec2,
    /// The file's own width and height, when it has them (not an SVG).
    pub size: Option<[u32; 2]>,
}

pub enum State<'a> {
    Loading,
    Ready(&'a Picture),
    Failed(&'a str),
}

struct Slot {
    wanted: Mutex<Option<Key>>,
    ready: Condvar,
}

type Decoded = Result<(ColorImage, Option<[u32; 2]>), String>;

pub struct Previews {
    slot: Arc<Slot>,
    /// The last one asked for, until it's back.
    pending: Option<Key>,
    done: Receiver<(Key, Decoded)>,
    kept: VecDeque<(Key, Result<Picture, String>)>,
}

impl Previews {
    pub fn new(ctx: &egui::Context) -> Self {
        let slot = Arc::new(Slot { wanted: Mutex::new(None), ready: Condvar::new() });
        let (send, done) = channel();
        let worker = slot.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new().name("preview".into()).spawn(move || work(&worker, &send, &ctx)).ok();
        Previews { slot, pending: None, done, kept: VecDeque::new() }
    }

    /// Takes in what the thread has finished.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((key, decoded)) = self.done.try_recv() {
            if self.pending.as_ref() == Some(&key) {
                self.pending = None;
            }
            let picture = decoded.map(|(image, size)| {
                let drawn = egui::vec2(image.size[0] as f32, image.size[1] as f32);
                let texture = ctx.load_texture(format!("preview-{}", key.0.display()), image, TextureOptions::LINEAR);
                Picture { texture, drawn, size }
            });
            self.kept.retain(|(k, _)| k != &key);
            self.kept.push_back((key, picture));
            while self.kept.len() > KEPT {
                self.kept.pop_front();
            }
        }
    }

    /// The picture at `path` fitting `px` pixels square, if it's ready; asks
    /// for it otherwise.
    pub fn get(&mut self, path: &Path, modified: Option<SystemTime>, px: u32) -> State<'_> {
        let key = (path.to_path_buf(), modified, px);
        if let Some(i) = self.kept.iter().position(|(k, _)| k == &key) {
            return match &self.kept[i].1 {
                Ok(picture) => State::Ready(picture),
                Err(e) => State::Failed(e),
            };
        }
        if self.pending.as_ref() != Some(&key) {
            *self.slot.wanted.lock().unwrap_or_else(|e| e.into_inner()) = Some(key.clone());
            self.slot.ready.notify_one();
            self.pending = Some(key);
        }
        State::Loading
    }
}

fn work(slot: &Slot, send: &Sender<(Key, Decoded)>, ctx: &egui::Context) {
    loop {
        let key = {
            let mut wanted = slot.wanted.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(key) = wanted.take() {
                    break key;
                }
                wanted = slot.ready.wait(wanted).unwrap_or_else(|e| e.into_inner());
            }
        };
        let decoded = decode(&key.0, key.2);
        if send.send((key, decoded)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// The picture at `path`, scaled down to fit `px` square if it's larger.
fn decode(path: &Path, px: u32) -> Decoded {
    let len = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if len > LIMIT {
        return Err("It's too large to preview.".into());
    }
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("svg")) {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let rgba = patina_apps::icon::svg(&bytes, px).ok_or("It can't be read as an SVG.")?;
        return Ok((ColorImage::from_rgba_premultiplied([px as usize, px as usize], &rgba), None));
    }
    let mut image = open(path)?;
    let size = [image.width(), image.height()];
    if image.width() > px || image.height() > px {
        image = image.thumbnail(px, px);
    }
    let rgba = image.into_rgba8();
    let drawn = [rgba.width() as usize, rgba.height() as usize];
    Ok((ColorImage::from_rgba_unmultiplied(drawn, &rgba), Some(size)))
}

/// A picture as its camera held it: turned or flipped as its EXIF says.
pub fn open(path: &Path) -> Result<DynamicImage, String> {
    let reader = image::ImageReader::open(path).and_then(|r| r.with_guessed_format()).map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    Ok(image)
}
