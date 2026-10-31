//! The text editor, for Edit on a script (ADR-0011): the desktop's app for
//! `text/plain` from the mimeapps lists, else `$VISUAL` or `$EDITOR` in a
//! terminal. xdg-open would run a script's own type's app, or the script.

use std::path::{Path, PathBuf};

use crate::mime::data_dirs;

const TEXT: &str = "text/plain";

/// The command line that edits `path`.
pub fn command(path: &Path) -> Result<Vec<String>, String> {
    if let Some(entry) = default_app(TEXT).and_then(|id| find_entry(&id))
        && let Ok(text) = std::fs::read_to_string(&entry)
        && let Some((exec, terminal)) = desktop_exec(&text)
    {
        let line = expand(&exec, path);
        if !line.is_empty() {
            return Ok(if terminal { in_terminal(line) } else { line });
        }
    }
    let editor = ["VISUAL", "EDITOR"]
        .into_iter()
        .find_map(|v| std::env::var(v).ok().filter(|e| !e.trim().is_empty()))
        .ok_or("there's no app for text, and neither $VISUAL nor $EDITOR is set")?;
    let mut line: Vec<String> = editor.split_whitespace().map(str::to_string).collect();
    line.push(path.display().to_string());
    Ok(in_terminal(line))
}

fn in_terminal(line: Vec<String>) -> Vec<String> {
    let mut out = vec![patina_apps::terminal(), "-e".into()];
    out.extend(line);
    out
}

/// The mimeapps lists, most important first, as the XDG MIME Applications
/// spec orders them.
fn lists() -> Vec<PathBuf> {
    let desktops: Vec<String> = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
        .filter(|d| !d.is_empty())
        .map(str::to_lowercase)
        .collect();
    let mut dirs: Vec<PathBuf> = dirs::config_dir().into_iter().collect();
    let system = std::env::var("XDG_CONFIG_DIRS").ok().filter(|d| !d.is_empty());
    dirs.extend(system.as_deref().unwrap_or("/etc/xdg").split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    dirs.extend(data_dirs().into_iter().map(|d| d.join("applications")));
    let mut lists = Vec::new();
    for dir in dirs {
        for desktop in &desktops {
            lists.push(dir.join(format!("{desktop}-mimeapps.list")));
        }
        lists.push(dir.join("mimeapps.list"));
        lists.push(dir.join("defaults.list"));
    }
    lists
}

/// The desktop entry id the lists give `mime` first, that's installed.
fn default_app(mime: &str) -> Option<String> {
    lists()
        .iter()
        .filter_map(|list| std::fs::read_to_string(list).ok())
        .find_map(|text| defaults(&text, mime).into_iter().find(|id| find_entry(id).is_some()))
}

/// The ids in a list's `[Default Applications]` for `mime`, in order.
fn defaults(text: &str, mime: &str) -> Vec<String> {
    let mut in_section = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line == "[Default Applications]";
        } else if in_section
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == mime
        {
            return value.split(';').map(str::trim).filter(|id| !id.is_empty()).map(str::to_string).collect();
        }
    }
    Vec::new()
}

fn find_entry(id: &str) -> Option<PathBuf> {
    data_dirs().into_iter().map(|d| d.join("applications").join(id)).find(|p| p.is_file())
}

/// A desktop entry's `Exec`, and whether it runs in a terminal.
fn desktop_exec(text: &str) -> Option<(String, bool)> {
    let mut in_entry = false;
    let (mut exec, mut terminal) = (None, false);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if in_entry && let Some((key, value)) = line.split_once('=') {
            match key.trim() {
                "Exec" => exec = Some(value.trim().to_string()),
                "Terminal" => terminal = value.trim() == "true",
                _ => {}
            }
        }
    }
    exec.map(|e| (e, terminal))
}

/// `Exec` split into words, its field codes filled in for `path`: the file
/// in place of `%f`, `%F`, `%u` or `%U`, or after the rest if there's none.
fn expand(exec: &str, path: &Path) -> Vec<String> {
    let file = path.display().to_string();
    let mut placed = false;
    let mut words = Vec::new();
    for word in split(exec) {
        match word.as_str() {
            "%f" | "%F" | "%u" | "%U" => {
                words.push(file.clone());
                placed = true;
            }
            // The icon, name and entry's location; and the deprecated codes.
            w if w.len() == 2 && w.starts_with('%') && w != "%%" => {}
            w => words.push(w.replace("%%", "%")),
        }
    }
    if !placed && !words.is_empty() {
        words.push(file);
    }
    words
}

/// Splits on spaces outside double quotes, where `\` escapes the next.
fn split(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let (mut quoted, mut started) = (false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => word.extend(chars.next()),
            ' ' | '\t' if !quoted => {
                if started || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
                started = false;
            }
            c => word.push(c),
        }
    }
    if started || !word.is_empty() {
        words.push(word);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_default_for_the_type() {
        let list = "[Added Associations]\ntext/plain=other.desktop;\n\n[Default Applications]\ntext/html=firefox.desktop\ntext/plain = nvim.desktop;gedit.desktop;\n";
        assert_eq!(defaults(list, "text/plain"), ["nvim.desktop", "gedit.desktop"]);
        assert!(defaults(list, "image/png").is_empty());
    }

    #[test]
    fn exec_and_terminal_from_the_desktop_entry() {
        let entry = "[Desktop Entry]\nName=Neovim\nExec=nvim %F\nTerminal=true\n\n[Desktop Action new]\nExec=other\n";
        assert_eq!(desktop_exec(entry), Some(("nvim %F".into(), true)));
    }

    #[test]
    fn field_codes_are_filled_in() {
        let path = Path::new("/home/me/run me.sh");
        assert_eq!(expand("gedit %U", path), ["gedit", "/home/me/run me.sh"]);
        assert_eq!(expand("kate -b %i", path), ["kate", "-b", "/home/me/run me.sh"]);
        assert_eq!(
            expand(r#""/opt/My Editor/ed" --new-window %f"#, path),
            ["/opt/My Editor/ed", "--new-window", "/home/me/run me.sh"]
        );
        assert_eq!(expand("ed --percent=100%%", path), ["ed", "--percent=100%", "/home/me/run me.sh"]);
    }
}
