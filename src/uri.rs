//! `file://` URIs, as other apps pass folders to open and as file managers
//! put copied files on the clipboard.

use std::path::{Path, PathBuf};

/// The path in `file:///a%20b`, or `text` as a path if it's one already.
pub fn to_path(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    if let Some(rest) = text.strip_prefix("file://") {
        // Skip a host, which is empty or "localhost".
        let path = &rest[rest.find('/')?..];
        return Some(PathBuf::from(decode(path)));
    }
    text.starts_with('/').then(|| PathBuf::from(text))
}

/// `path` as a `file://` URI, with what isn't safe in one escaped.
pub fn from_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~!$&'()*+,;=:@".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

fn decode(text: &str) -> std::ffi::OsString {
    use std::os::unix::ffi::OsStringExt;
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        let digit = |at: usize| bytes.get(at).and_then(|&b| hex(b));
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (digit(i + 1), digit(i + 2))
        {
            out.push((h * 16 + l) as u8);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    std::ffi::OsString::from_vec(out)
}

/// Files on the clipboard: Nautilus's text (`x-special/nautilus-clipboard`,
/// then `copy` or `cut`, then URIs), or plain paths and URIs one a line.
/// Says whether they were cut, and keeps only paths that exist.
pub fn clipboard_files(text: &str) -> (Vec<PathBuf>, bool) {
    let mut lines = text.lines().peekable();
    let mut cut = false;
    if lines.peek() == Some(&"x-special/nautilus-clipboard") {
        lines.next();
        cut = lines.next() == Some("cut");
    }
    let paths = lines.filter_map(to_path).filter(|p| std::fs::symlink_metadata(p).is_ok()).collect();
    (paths, cut)
}

/// `paths` as a `text/uri-list`, as a drag hands them to another app:
/// one URI a line, each ended with CR LF (RFC 2483).
pub fn drag_list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| from_path(p) + "\r\n").collect()
}

/// What Files puts on the clipboard: Nautilus's form, so Nautilus pastes it.
pub fn clipboard_text(paths: &[PathBuf], cut: bool) -> String {
    let mut text = format!("x-special/nautilus-clipboard\n{}\n", if cut { "cut" } else { "copy" });
    for path in paths {
        text.push_str(&from_path(path));
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_list_has_a_crlf_line_per_file() {
        let list = drag_list(&[PathBuf::from("/home/a/b c.png"), PathBuf::from("/tmp/x")]);
        assert_eq!(list, "file:///home/a/b%20c.png\r\nfile:///tmp/x\r\n");
    }

    #[test]
    fn uris_and_paths_round_trip() {
        let path = Path::new("/home/me/My Files/ü #1.txt");
        let uri = from_path(path);
        assert_eq!(uri, "file:///home/me/My%20Files/%C3%BC%20%231.txt");
        assert_eq!(to_path(&uri).unwrap(), path);
        assert_eq!(to_path("file://localhost/tmp").unwrap(), Path::new("/tmp"));
        assert_eq!(to_path("/tmp").unwrap(), Path::new("/tmp"));
        assert_eq!(to_path("tmp"), None);
        assert_eq!(to_path("file:///50%").unwrap(), Path::new("/50%"));
    }

    #[test]
    fn reads_nautilus_clipboard_and_plain_paths() {
        let text = clipboard_text(&[PathBuf::from("/tmp")], true);
        assert_eq!(text, "x-special/nautilus-clipboard\ncut\nfile:///tmp\n");
        assert_eq!(clipboard_files(&text), (vec![PathBuf::from("/tmp")], true));
        assert_eq!(clipboard_files("/tmp\n/no/such/thing\n"), (vec![PathBuf::from("/tmp")], false));
        assert_eq!(clipboard_files("hello"), (vec![], false));
    }
}
