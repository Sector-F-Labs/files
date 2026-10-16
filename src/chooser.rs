//! What a file chooser is asked and what it answers: Files' window in a
//! picking mode, started by the portal (`portal.rs`) as `files --choose`
//! with a `Request` as TOML on stdin, printing an `Answer` as TOML on
//! stdout (ADR-0004).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A file type the app offers, as the portal gives them: globs such as
/// `*.png` and MIME types such as `image/*`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    pub name: String,
    pub globs: Vec<String>,
    pub mimes: Vec<String>,
}

impl Filter {
    /// Whether a file named `name`, of type `mime` if known, is one.
    pub fn matches(&self, name: &str, mime: Option<&str>) -> bool {
        let name = name.to_lowercase();
        self.globs.iter().any(|g| glob(&g.to_lowercase(), &name))
            || mime.is_some_and(|m| {
                self.mimes.iter().any(|want| match want.strip_suffix("/*") {
                    Some(top) => m.split('/').next() == Some(top),
                    None => want == m || want == "*" || want == "*/*",
                })
            })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Request {
    /// Saving rather than opening.
    pub save: bool,
    pub title: String,
    /// The accept button's label; "Open" or "Save" without one.
    pub accept_label: Option<String>,
    /// Choosing several files.
    pub multiple: bool,
    /// Choosing folders rather than files.
    pub directory: bool,
    pub filters: Vec<Filter>,
    /// Which filter starts chosen, an index into `filters`.
    pub current_filter: Option<usize>,
    /// Where to start.
    pub folder: Option<PathBuf>,
    /// For saving: the name to suggest, or the file being saved again.
    pub name: Option<String>,
    pub file: Option<PathBuf>,
}

impl Request {
    pub fn accept_label(&self) -> String {
        let label = self.accept_label.clone().filter(|l| !l.is_empty());
        // GTK labels carry mnemonics, "_Open"; drop the underscore.
        let label = label.map(|l| l.replacen('_', "", 1));
        label.unwrap_or_else(|| if self.save { "Save" } else { "Open" }.into())
    }

    pub fn title(&self) -> String {
        if !self.title.is_empty() {
            return self.title.clone();
        }
        match (self.save, self.directory) {
            (true, _) => "Save",
            (false, true) => "Choose a Folder",
            (false, false) if self.multiple => "Open Files",
            (false, false) => "Open a File",
        }
        .into()
    }

    /// The folder to start in, if the request names one that exists.
    pub fn start(&self) -> Option<PathBuf> {
        let from_file = self.file.as_deref().and_then(Path::parent);
        from_file.or(self.folder.as_deref()).filter(|f| f.is_dir()).map(Path::to_path_buf)
    }

    /// The name to start the save field with.
    pub fn start_name(&self) -> String {
        let from_file = self.file.as_deref().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned());
        from_file.or_else(|| self.name.clone()).unwrap_or_default()
    }
}

/// What was chosen, and in which filter.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Answer {
    pub paths: Vec<PathBuf>,
    pub filter: Option<usize>,
}

/// Matches `text` against a glob of `*`, `?` and `[...]` classes.
fn glob(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    fn at(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|skip| at(&p[1..], &t[skip..])),
            Some('?') => !t.is_empty() && at(&p[1..], &t[1..]),
            Some('[') => {
                let Some(end) = p.iter().position(|&c| c == ']').filter(|&e| e > 1) else {
                    return t.first() == Some(&'[') && at(&p[1..], &t[1..]);
                };
                let Some(&c) = t.first() else { return false };
                let class = &p[1..end];
                let (negate, class) = match class.first() {
                    Some('!' | '^') => (true, &class[1..]),
                    _ => (false, class),
                };
                let mut hit = false;
                let mut i = 0;
                while i < class.len() {
                    if i + 2 < class.len() && class[i + 1] == '-' {
                        hit |= class[i] <= c && c <= class[i + 2];
                        i += 3;
                    } else {
                        hit |= class[i] == c;
                        i += 1;
                    }
                }
                hit != negate && at(&p[end + 1..], &t[1..])
            }
            Some(&c) => t.first() == Some(&c) && at(&p[1..], &t[1..]),
        }
    }
    at(&p, &t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs_match_as_gtk_writes_them() {
        assert!(glob("*.png", "a.png"));
        assert!(!glob("*.png", "a.png.txt"));
        assert!(glob("*.[pj]ng", "x.jng"));
        assert!(glob("*.[a-c]", "x.b"));
        assert!(!glob("*.[!a-c]", "x.b"));
        assert!(glob("file?.txt", "file1.txt"));
        assert!(glob("*", ""));
        let images = Filter { name: "Images".into(), globs: vec!["*.PNG".into()], mimes: vec!["image/*".into()] };
        assert!(images.matches("A.png", None), "case-insensitive");
        assert!(images.matches("photo.jpg", Some("image/jpeg")));
        assert!(!images.matches("notes.txt", Some("text/plain")));
    }

    #[test]
    fn requests_round_trip_and_fill_in() {
        let request = Request {
            save: true,
            file: Some("/tmp/report.txt".into()),
            filters: vec![Filter { name: "Text".into(), globs: vec!["*.txt".into()], mimes: vec![] }],
            accept_label: Some("_Export".into()),
            ..Request::default()
        };
        let text = toml::to_string(&request).unwrap();
        assert_eq!(toml::from_str::<Request>(&text).unwrap(), request);
        assert_eq!(request.accept_label(), "Export");
        assert_eq!(request.start_name(), "report.txt");
        assert_eq!(request.start(), Some(PathBuf::from("/tmp")));
        assert_eq!(Request::default().title(), "Open a File");
    }
}
