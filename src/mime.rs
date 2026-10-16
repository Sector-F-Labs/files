//! File types by name, from the shared MIME-info database the rest of the
//! desktop uses (`globs2`), and the icon names the icon theme spec gives
//! them (`icons`, `generic-icons`). Reading the files' contents isn't
//! attempted: a name without a known extension is a plain file.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Default)]
pub struct Mime {
    /// Lower-case extension (`tar.gz`) to type, with the glob's weight.
    extensions: HashMap<String, (u32, String)>,
    /// Whole names (`Makefile`) to type.
    names: HashMap<String, String>,
    /// Type to icon name, where it isn't the type with `/` as `-`.
    icons: HashMap<String, String>,
    /// Type to the generic icon of its kind (`image-x-generic`).
    generic: HashMap<String, String>,
    /// Descriptions read so far, from `mime/<type>.xml`.
    comments: Mutex<HashMap<String, String>>,
}

impl Mime {
    /// The database in every XDG data directory, the user's own first.
    pub fn load() -> Self {
        let mut mime = Mime::default();
        // Later directories are read first, so earlier ones override them.
        for dir in data_dirs().iter().rev() {
            let dir = dir.join("mime");
            if let Ok(text) = std::fs::read_to_string(dir.join("globs2")) {
                mime.add_globs(&text);
            }
            for (file, map) in [("icons", &mut mime.icons), ("generic-icons", &mut mime.generic)] {
                if let Ok(text) = std::fs::read_to_string(dir.join(file)) {
                    for (ty, icon) in text.lines().filter_map(|l| l.split_once(':')) {
                        map.insert(ty.to_string(), icon.to_string());
                    }
                }
            }
        }
        mime
    }

    /// Lines of `weight:type:glob[:flags]`; only `*.ext` and plain names.
    fn add_globs(&mut self, text: &str) {
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            let mut fields = line.split(':');
            let (Some(weight), Some(ty), Some(glob)) = (fields.next(), fields.next(), fields.next()) else {
                continue;
            };
            let weight = weight.parse().unwrap_or(50);
            let wild = |s: &str| s.contains(['*', '?', '[']);
            if let Some(ext) = glob.strip_prefix("*.") {
                if wild(ext) {
                    continue;
                }
                let ext = ext.to_lowercase();
                let better = self.extensions.get(&ext).is_none_or(|(w, _)| weight > *w);
                if better {
                    self.extensions.insert(ext, (weight, ty.to_string()));
                }
            } else if !wild(glob) {
                self.names.insert(glob.to_string(), ty.to_string());
            }
        }
    }

    /// The type of a file named `name`, by its longest known extension.
    pub fn guess(&self, name: &str) -> Option<&str> {
        if let Some(ty) = self.names.get(name) {
            return Some(ty);
        }
        let lower = name.to_lowercase();
        lower
            .match_indices('.')
            .filter(|(i, _)| *i > 0)
            .find_map(|(i, _)| self.extensions.get(&lower[i + 1..]))
            .map(|(_, ty)| ty.as_str())
    }

    /// Icon names for `ty`, most specific first.
    pub fn icon_names(&self, ty: &str) -> Vec<String> {
        let own = self.icons.get(ty).cloned().unwrap_or_else(|| ty.replace('/', "-"));
        let top = ty.split('/').next().unwrap_or_default();
        let generic = self.generic.get(ty).cloned().unwrap_or_else(|| format!("{top}-x-generic"));
        vec![own, generic, "text-x-generic".into()]
    }

    /// What `ty` is called, for the Kind column: the database's comment
    /// ("PNG image"), or one made from the type where it has none.
    pub fn describe(&self, ty: &str) -> String {
        let mut cache = self.comments.lock().unwrap_or_else(|e| e.into_inner());
        cache.entry(ty.to_string()).or_insert_with(|| comment(ty).unwrap_or_else(|| Self::kind(ty))).clone()
    }

    fn kind(ty: &str) -> String {
        let (top, sub) = ty.split_once('/').unwrap_or((ty, ""));
        let sub = sub.trim_start_matches("x-").trim_start_matches("vnd.");
        match top {
            "image" | "audio" | "video" | "text" | "font" => {
                format!("{} {top}", sub.to_uppercase())
            }
            _ => format!("{} document", sub.to_uppercase()),
        }
    }
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`, as the spec defaults them.
pub fn data_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = dirs::data_dir() {
        dirs.push(home);
    }
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(system.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs
}

/// The English `<comment>` in the type's own file, `mime/image/png.xml`.
fn comment(ty: &str) -> Option<String> {
    data_dirs().into_iter().find_map(|dir| {
        let xml = std::fs::read_to_string(dir.join("mime").join(format!("{ty}.xml"))).ok()?;
        let start = xml.find("<comment>")? + "<comment>".len();
        let end = start + xml[start..].find("</comment>")?;
        Some(unescape(&xml[start..end]))
    })
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Whether `meta` is an executable file's.
pub fn executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.is_file() && meta.permissions().mode() & 0o111 != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Mime {
        let mut mime = Mime::default();
        mime.add_globs(
            "# comment\n50:image/png:*.png\n50:application/gzip:*.gz\n\
             55:application/x-compressed-tar:*.tar.gz\n50:text/x-makefile:Makefile\n\
             50:text/x-c:*.[ch]\n40:text/plain:*.PNG\n",
        );
        mime.generic.insert("application/x-compressed-tar".into(), "package-x-generic".into());
        mime
    }

    #[test]
    fn guesses_by_the_longest_extension_and_by_name() {
        let mime = sample();
        assert_eq!(mime.guess("a.png"), Some("image/png"));
        assert_eq!(mime.guess("A.PNG"), Some("image/png"), "case-insensitive, heavier glob wins");
        assert_eq!(mime.guess("src.tar.gz"), Some("application/x-compressed-tar"));
        assert_eq!(mime.guess("x.gz"), Some("application/gzip"));
        assert_eq!(mime.guess("Makefile"), Some("text/x-makefile"));
        assert_eq!(mime.guess(".png"), None, "a hidden name isn't an extension");
        assert_eq!(mime.guess("main.c"), None, "wildcard globs are skipped");
        assert_eq!(mime.guess("README"), None);
    }

    #[test]
    fn icon_names_go_from_specific_to_generic() {
        let mime = sample();
        assert_eq!(mime.icon_names("image/png"), ["image-png", "image-x-generic", "text-x-generic"]);
        assert_eq!(mime.icon_names("application/x-compressed-tar")[1], "package-x-generic");
    }

    #[test]
    fn kinds_read_as_words() {
        assert_eq!(Mime::kind("image/png"), "PNG image");
        assert_eq!(Mime::kind("application/x-compressed-tar"), "COMPRESSED-TAR document");
    }
}
