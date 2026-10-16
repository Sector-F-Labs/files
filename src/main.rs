//! Files: a Patina file manager. Folders as icons or a list, in tabs, with
//! copying, moving, the Trash and renaming; and a `Name.app` folder is an
//! app, shown with its own icon and run when it's opened, as on a Mac
//! (patina-apps).

mod app;
mod config;
mod glyphs;
mod icons;
mod listing;
mod mime;
mod ops;
mod places;
mod uri;
mod watch;

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: files [FOLDER | FILE | URI]

Opens a window on FOLDER (default: home), or on the folder holding FILE with
it selected. file:// URIs are understood too.";

/// The icon, drawn for the window from the same SVG the launcher shows.
const ICON_SVG: &[u8] = include_bytes!("../packaging/linux/files.svg");

fn window_icon() -> egui::IconData {
    const SIZE: u32 = 128;
    let rgba = patina_apps::icon::svg(ICON_SVG, SIZE).expect("the icon is a valid SVG");
    // tiny-skia renders premultiplied; the window icon wants it straight.
    let rgba = rgba
        .chunks_exact(4)
        .flat_map(|p| {
            let a = p[3] as u16;
            let un = |c: u8| (c as u16 * 255 + a / 2).checked_div(a).map_or(0, |v| v.min(255) as u8);
            [un(p[0]), un(p[1]), un(p[2]), p[3]]
        })
        .collect();
    egui::IconData { rgba, width: SIZE, height: SIZE }
}

fn main() -> ExitCode {
    let mut start = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("files {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            _ if start.is_none() => {
                start = uri::to_path(&arg).or_else(|| std::path::absolute(PathBuf::from(&arg)).ok());
            }
            _ => {}
        }
    }
    let options = eframe::NativeOptions {
        viewport: patina::frame::viewport(
            egui::ViewportBuilder::default()
                .with_title("Files")
                .with_app_id("com.sectorflabs.Files")
                .with_icon(window_icon())
                .with_inner_size([960.0, 620.0])
                .with_min_inner_size([520.0, 340.0]),
            true,
        ),
        ..Default::default()
    };
    match eframe::run_native("files", options, Box::new(|cc| Ok(Box::new(app::Files::new(cc, start))))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("files: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn window_icon_draws() {
        let icon = super::window_icon();
        assert_eq!((icon.width, icon.height), (128, 128));
        assert!(icon.rgba.chunks(4).any(|p| p[3] == 255));
    }
}
