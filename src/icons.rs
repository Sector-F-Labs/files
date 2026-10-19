//! Icons for what's shown: the theme's icon for a file's type or a place,
//! an app's own icon from its folder, or a picture's thumbnail. Looked up
//! and decoded on a thread, newest request first, so the items on screen
//! fill in before ones scrolled past; each is a texture once it's ready.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;

use egui::{ColorImage, TextureHandle, TextureId, TextureOptions};

/// What an icon is drawn from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Source {
    /// Theme icon names, the first found wins.
    Theme(Vec<String>),
    /// An app folder's own icon, or the executable icon.
    App(PathBuf),
    /// A picture drawn small, or the theme's icon for its type if it can't
    /// be decoded. The time it was changed makes a new thumbnail.
    Thumbnail(PathBuf, Option<SystemTime>, Vec<String>),
}

/// The theme's icon for something, by its names, most specific first.
pub fn theme(names: &[&str]) -> Source {
    Source::Theme(names.iter().map(|n| n.to_string()).collect())
}

/// Pictures larger than this get the type's icon instead of a thumbnail.
const THUMBNAIL_LIMIT: u64 = 40_000_000;

type Key = (Source, u32);

struct Queue {
    wanted: Mutex<Vec<Key>>,
    ready: Condvar,
}

pub struct Icons {
    textures: HashMap<Key, Option<TextureHandle>>,
    /// The same, as pixels: a drag's icon shows one at once (ADR-0006).
    images: HashMap<Key, Arc<ColorImage>>,
    queue: Arc<Queue>,
    done: Receiver<(Key, Option<ColorImage>)>,
}

impl Icons {
    pub fn new(ctx: &egui::Context) -> Self {
        let queue = Arc::new(Queue { wanted: Mutex::new(Vec::new()), ready: Condvar::new() });
        let (send, done) = channel();
        let theme = icon_theme();
        let worker = queue.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new().name("icons".into()).spawn(move || work(&worker, &theme, &send, &ctx)).ok();
        Icons { textures: HashMap::new(), images: HashMap::new(), queue, done }
    }

    /// Takes in what the thread has finished.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((key, image)) = self.done.try_recv() {
            let name = format!("icon-{}", self.textures.len());
            if let Some(i) = &image {
                self.images.insert(key.clone(), Arc::new(i.clone()));
            }
            let texture = image.map(|i| ctx.load_texture(name, i, TextureOptions::LINEAR));
            self.textures.insert(key, texture);
        }
    }

    /// The largest picture of `source` drawn so far.
    pub fn drawn(&self, source: &Source) -> Option<Arc<ColorImage>> {
        self.images.iter().filter(|((s, _), _)| s == source).max_by_key(|((_, px), _)| *px).map(|(_, i)| i.clone())
    }

    /// The texture for `source` at `px` pixels, if it's ready; asks for it
    /// otherwise.
    pub fn get(&mut self, source: &Source, px: u32) -> Option<TextureId> {
        let key = (source.clone(), px);
        if let Some(texture) = self.textures.get(&key) {
            return texture.as_ref().map(|t| t.id());
        }
        let mut wanted = self.queue.wanted.lock().unwrap_or_else(|e| e.into_inner());
        if !wanted.contains(&key) {
            wanted.push(key);
            self.queue.ready.notify_one();
        }
        None
    }
}

fn work(queue: &Queue, theme: &str, send: &Sender<(Key, Option<ColorImage>)>, ctx: &egui::Context) {
    loop {
        let key = {
            let mut wanted = queue.wanted.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if let Some(key) = wanted.pop() {
                    break key;
                }
                wanted = queue.ready.wait(wanted).unwrap_or_else(|e| e.into_inner());
            }
        };
        let image = draw(&key.0, key.1, theme);
        if send.send((key, image)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

/// `source` drawn at `px` pixels, there and then: for the icon a drag
/// out of the window shows, on a thread of its own.
pub fn image(source: &Source, px: u32) -> Option<ColorImage> {
    draw(source, px, &icon_theme())
}

fn draw(source: &Source, px: u32, theme: &str) -> Option<ColorImage> {
    let rgba = match source {
        Source::Theme(names) => from_theme(names, px, theme),
        Source::App(folder) => patina_apps::icon(folder)
            .and_then(|path| patina_apps::icon::render(&path, px))
            .or_else(|| from_theme(&["application-x-executable".into()], px, theme)),
        Source::Thumbnail(path, _, names) => thumbnail(path, px).or_else(|| from_theme(names, px, theme)),
    }?;
    Some(ColorImage::from_rgba_premultiplied([px as usize, px as usize], &rgba))
}

fn from_theme(names: &[String], px: u32, theme: &str) -> Option<Vec<u8>> {
    names.iter().find_map(|name| {
        let path = freedesktop_icons::lookup(name)
            .with_size(px.min(u16::MAX as u32) as u16)
            .with_theme(theme)
            .with_cache()
            .find()?;
        patina_apps::icon::render(&path, px)
    })
}

/// A picture scaled to fit `px` square, centred, as premultiplied RGBA.
fn thumbnail(path: &std::path::Path, px: u32) -> Option<Vec<u8>> {
    if std::fs::metadata(path).ok()?.len() > THUMBNAIL_LIMIT {
        return None;
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if ext == "svg" {
        return patina_apps::icon::svg(&std::fs::read(path).ok()?, px);
    }
    let image = crate::preview::open(path).ok()?;
    let fitted = image.thumbnail(px, px).into_rgba8();
    let (w, h) = fitted.dimensions();
    let mut square = image::RgbaImage::new(px, px);
    image::imageops::overlay(&mut square, &fitted, ((px - w) / 2) as i64, ((px - h) / 2) as i64);
    Some(
        square
            .pixels()
            .flat_map(|p| {
                let [r, g, b, a] = p.0;
                let m = |c: u8| ((c as u16 * a as u16 + 127) / 255) as u8;
                [m(r), m(g), m(b), a]
            })
            .collect(),
    )
}

/// The desktop's icon theme: GNOME's setting where there is one and it's
/// installed, else Adwaita, falling back to hicolor as the spec says. A
/// setting left behind by a removed desktop (breeze-dark, once KDE went)
/// would leave only hicolor, which has no folders or file types.
fn icon_theme() -> String {
    std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_matches('\'').to_string())
        .filter(|t| !t.is_empty() && installed(t, &icon_dirs()))
        .unwrap_or_else(|| "Adwaita".into())
}

/// Where icon themes are, as the spec looks: `~/.icons`, then `icons` in
/// each XDG data directory.
fn icon_dirs() -> Vec<std::path::PathBuf> {
    let env = |name: &str| std::env::var_os(name).filter(|v| !v.is_empty());
    let home = env("HOME").map(std::path::PathBuf::from);
    let data_home = env("XDG_DATA_HOME").map(std::path::PathBuf::from).or_else(|| home.as_ref().map(|h| h.join(".local/share")));
    let data_dirs = env("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.map(|h| h.join(".icons"))
        .into_iter()
        .chain(data_home.map(|d| d.join("icons")))
        .chain(std::env::split_paths(&data_dirs).map(|d| d.join("icons")))
        .collect()
}

/// Whether theme `name` is in one of `dirs`: a folder of that name with an
/// index.theme.
fn installed(name: &str, dirs: &[std::path::PathBuf]) -> bool {
    dirs.iter().any(|d| d.join(name).join("index.theme").is_file())
}

/// Whether a file of type `mime` gets a thumbnail.
pub fn has_thumbnail(mime: &str) -> bool {
    matches!(mime, "image/png" | "image/jpeg" | "image/gif" | "image/webp" | "image/bmp" | "image/svg+xml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_counts_only_where_its_index_is() {
        let dir = std::env::temp_dir().join(format!("files-icons-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Adwaita")).unwrap();
        std::fs::write(dir.join("Adwaita/index.theme"), "[Icon Theme]\n").unwrap();
        std::fs::create_dir_all(dir.join("breeze-dark")).unwrap();
        let dirs = [dir.clone()];
        assert!(installed("Adwaita", &dirs));
        assert!(!installed("breeze-dark", &dirs), "a folder left behind, without its index");
        assert!(!installed("Yaru", &dirs));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
