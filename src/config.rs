//! Files' settings, in `~/.config/files/config.toml`, saved when they
//! change.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::listing::{Sort, SortBy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum View {
    Icons,
    List,
    /// A column for each folder down to this one, as a Mac's Finder has.
    Columns,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub view: View,
    pub show_hidden: bool,
    pub sort: Sort,
    /// Folders the user added to the sidebar.
    pub favourites: Vec<PathBuf>,
    /// The list view's columns after Name (which is always there).
    pub list_columns: Vec<SortBy>,
    /// The desktop's title bar instead of Patina's.
    pub system_title_bar: bool,
    pub skin: String,
    /// "system", "light" or "dark", while the skin follows the desktop.
    pub light_dark: String,
    /// Where the last file was chosen for an app, to start the next there.
    pub chooser_folder: Option<PathBuf>,
    /// The ZFS side panel is open.
    pub zfs_panel: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            view: View::Icons,
            show_hidden: false,
            sort: Sort::default(),
            favourites: Vec::new(),
            list_columns: vec![SortBy::Size, SortBy::Modified, SortBy::Kind],
            system_title_bar: false,
            skin: patina::skin::DEFAULT_SKIN.into(),
            light_dark: "system".into(),
            chooser_folder: None,
            zfs_panel: false,
        }
    }
}

fn path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("files/config.toml"))
}

impl Config {
    /// The saved settings; the defaults if there are none or they don't read.
    pub fn load() -> Self {
        let Some(text) = path().and_then(|p| std::fs::read_to_string(p).ok()) else {
            return Config::default();
        };
        toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("files: config.toml: {e}");
            Config::default()
        })
    }

    pub fn save(&self) {
        let Some(path) = path() else { return };
        let saved = toml::to_string(self).map_err(|e| e.to_string()).and_then(|text| {
            std::fs::create_dir_all(path.parent().unwrap_or(&path)).map_err(|e| e.to_string())?;
            std::fs::write(&path, text).map_err(|e| e.to_string())
        });
        if let Err(e) = saved {
            eprintln!("files: can't save {}: {e}", path.display());
        }
    }

    pub fn theme(&self) -> egui::ThemePreference {
        match self.light_dark.as_str() {
            "light" => egui::ThemePreference::Light,
            "dark" => egui::ThemePreference::Dark,
            _ => egui::ThemePreference::System,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_fills_in_what_is_missing() {
        let config = Config { view: View::List, show_hidden: true, ..Config::default() };
        let text = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<Config>(&text).unwrap(), config);
        let partial: Config = toml::from_str("show_hidden = true\n").unwrap();
        assert_eq!(partial.view, View::Icons);
        assert!(partial.show_hidden);
    }
}
