//! `files --portal`: Patina's file chooser for every app that asks the
//! desktop portal for one (`org.freedesktop.impl.portal.FileChooser`,
//! ADR-0004). xdg-desktop-portal hands us each request; we start
//! `files --choose` with it, wait for the answer and hand that back. A
//! process per dialog, since a process can only run one window loop.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use crate::chooser::{Answer, Filter, Request};
use crate::uri;

/// The name and path xdg-desktop-portal looks for, from `patina.portal`.
pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.patina";
const PATH: &str = "/org/freedesktop/portal/desktop";

/// The portal's answers: chosen, cancelled, or ended some other way.
const SUCCESS: u32 = 0;
const CANCELLED: u32 = 1;
const OTHER: u32 = 2;

type Results = HashMap<String, OwnedValue>;

/// How long the service waits with no dialog open before it exits; D-Bus
/// starts it again for the next request.
const IDLE: Duration = Duration::from_secs(300);

pub fn run() -> ExitCode {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("files"));
    let open = Arc::new(AtomicUsize::new(0));
    let last = Arc::new(Mutex::new(Instant::now()));
    let chooser = FileChooser { exe, open: open.clone(), last: last.clone() };
    let served = zbus::blocking::connection::Builder::session()
        .and_then(|b| b.name(BUS_NAME))
        .and_then(|b| b.serve_at(PATH, chooser))
        .and_then(|b| b.build());
    match served {
        Ok(_connection) => loop {
            std::thread::sleep(Duration::from_secs(10));
            let idle = last.lock().unwrap_or_else(|e| e.into_inner()).elapsed();
            if open.load(Ordering::SeqCst) == 0 && idle > IDLE {
                return ExitCode::SUCCESS;
            }
        },
        Err(e) => {
            eprintln!("files: can't serve {BUS_NAME}: {e}");
            ExitCode::FAILURE
        }
    }
}

struct FileChooser {
    exe: PathBuf,
    /// Dialogs open, and when the last one closed, for exiting when idle.
    open: Arc<AtomicUsize>,
    last: Arc<Mutex<Instant>>,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
impl FileChooser {
    async fn open_file(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let request = Request {
            save: false,
            title,
            multiple: flag(&options, "multiple"),
            directory: flag(&options, "directory"),
            ..common(&options)
        };
        let filters = request.filters.clone();
        match self.choose(server, handle, request).await {
            Ok(answer) => (SUCCESS, results(&answer.paths, &filters, answer.filter)),
            Err(response) => (response, Results::new()),
        }
    }

    async fn save_file(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let request = Request {
            save: true,
            title,
            name: text(&options, "current_name"),
            file: bytes_path(&options, "current_file"),
            ..common(&options)
        };
        let filters = request.filters.clone();
        match self.choose(server, handle, request).await {
            Ok(answer) => (SUCCESS, results(&answer.paths, &filters, answer.filter)),
            Err(response) => (response, Results::new()),
        }
    }

    /// Saving several files: a folder is chosen, and each goes in it.
    async fn save_files(
        &self,
        #[zbus(object_server)] server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        _app_id: String,
        _parent_window: String,
        title: String,
        options: HashMap<String, OwnedValue>,
    ) -> (u32, Results) {
        let names: Vec<PathBuf> = options
            .get("files")
            .and_then(|v| <Vec<Vec<u8>>>::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|b| path_from_bytes(&b))
            .collect();
        let request = Request { save: false, directory: true, title, ..common(&options) };
        match self.choose(server, handle, request).await {
            Ok(answer) => {
                let Some(folder) = answer.paths.first() else { return (CANCELLED, Results::new()) };
                let paths: Vec<PathBuf> = names.iter().filter_map(|n| n.file_name()).map(|n| folder.join(n)).collect();
                (SUCCESS, results(&paths, &[], None))
            }
            Err(response) => (response, Results::new()),
        }
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        4
    }
}

impl FileChooser {
    /// Shows the dialog for `request` until it's answered or the portal
    /// closes it: the answer, or the response to give instead.
    async fn choose(
        &self,
        server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        request: Request,
    ) -> Result<Answer, u32> {
        self.open.fetch_add(1, Ordering::SeqCst);
        let answer = self.show(server, handle, request).await;
        self.open.fetch_sub(1, Ordering::SeqCst);
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        answer
    }

    async fn show(
        &self,
        server: &zbus::ObjectServer,
        handle: OwnedObjectPath,
        request: Request,
    ) -> Result<Answer, u32> {
        let text = toml::to_string(&request).map_err(|_| OTHER)?;
        let mut child =
            Command::new(&self.exe).arg("--choose").stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().map_err(
                |e| {
                    eprintln!("files: can't start the chooser: {e}");
                    OTHER
                },
            )?;
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let pid = child.id();
        let _ = server.at(&handle, Closer { pid }).await;
        let output = blocking::unblock(move || child.wait_with_output()).await;
        let _ = server.remove::<Closer, _>(&handle).await;
        let output = output.map_err(|_| OTHER)?;
        if !output.status.success() {
            return Err(if output.status.code() == Some(1) { CANCELLED } else { OTHER });
        }
        toml::from_str(&String::from_utf8_lossy(&output.stdout)).map_err(|_| OTHER)
    }
}

/// The request object the portal calls Close on when the app gives up.
struct Closer {
    pid: u32,
}

#[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
impl Closer {
    fn close(&self) {
        // SAFETY: kill(2) on our own child's pid; no memory is involved.
        unsafe {
            libc::kill(self.pid as libc::pid_t, libc::SIGTERM);
        }
    }
}

/// What OpenFile and SaveFile share: the accept label, filters, and the
/// folder to start in.
fn common(options: &HashMap<String, OwnedValue>) -> Request {
    let filters: Vec<Filter> = options
        .get("filters")
        .and_then(|v| <Vec<(String, Vec<(u32, String)>)>>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default()
        .into_iter()
        .map(filter)
        .collect();
    let current = options
        .get("current_filter")
        .and_then(|v| <(String, Vec<(u32, String)>)>::try_from(v.try_clone().ok()?).ok())
        .map(filter);
    let mut request = Request {
        accept_label: text(options, "accept_label"),
        folder: bytes_path(options, "current_folder"),
        filters,
        ..Request::default()
    };
    if let Some(current) = current {
        let at = request.filters.iter().position(|f| *f == current);
        request.current_filter = Some(at.unwrap_or_else(|| {
            request.filters.push(current);
            request.filters.len() - 1
        }));
    }
    request
}

/// A portal filter, `(name, [(0, glob) | (1, mime type)])`.
fn filter((name, rules): (String, Vec<(u32, String)>)) -> Filter {
    let mut filter = Filter { name, ..Filter::default() };
    for (kind, rule) in rules {
        if kind == 0 { filter.globs.push(rule) } else { filter.mimes.push(rule) }
    }
    filter
}

fn flag(options: &HashMap<String, OwnedValue>, key: &str) -> bool {
    options.get(key).and_then(|v| bool::try_from(v).ok()).unwrap_or(false)
}

fn text(options: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    options.get(key).and_then(|v| <&str>::try_from(v).ok()).map(str::to_string)
}

/// A path given as bytes ending in a NUL (`ay`).
fn bytes_path(options: &HashMap<String, OwnedValue>, key: &str) -> Option<PathBuf> {
    let bytes = <Vec<u8>>::try_from(options.get(key)?.try_clone().ok()?).ok()?;
    let path = path_from_bytes(&bytes);
    (!path.as_os_str().is_empty()).then_some(path)
}

fn path_from_bytes(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    PathBuf::from(std::ffi::OsStr::from_bytes(&bytes[..end]))
}

/// The portal's results: the chosen files as URIs, and the filter in use.
fn results(paths: &[PathBuf], filters: &[Filter], current: Option<usize>) -> Results {
    let mut results = Results::new();
    let uris: Vec<String> = paths.iter().map(|p| uri::from_path(p)).collect();
    if let Ok(v) = OwnedValue::try_from(Value::from(uris)) {
        results.insert("uris".into(), v);
    }
    if let Some(filter) = current.and_then(|i| filters.get(i)) {
        let rules: Vec<(u32, String)> =
            filter.globs.iter().map(|g| (0, g.clone())).chain(filter.mimes.iter().map(|m| (1, m.clone()))).collect();
        if let Ok(v) = OwnedValue::try_from(Value::new((filter.name.clone(), rules))) {
            results.insert("current_filter".into(), v);
        }
    }
    if let Ok(v) = OwnedValue::try_from(Value::from(true)) {
        results.insert("writable".into(), v);
    }
    results
}

/// For `files --choose`: reads the request from stdin.
pub fn read_request() -> Result<Request, String> {
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut text).map_err(|e| e.to_string())?;
    toml::from_str(&text).map_err(|e| e.to_string())
}

/// For `files --choose`: prints the answer for the portal.
pub fn print_answer(answer: &Answer) {
    if let Ok(text) = toml::to_string(answer) {
        print!("{text}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn value<T: Into<Value<'static>>>(v: T) -> OwnedValue {
        OwnedValue::try_from(v.into()).unwrap()
    }

    #[test]
    fn reads_the_portals_options() {
        let mut options = HashMap::new();
        options.insert("multiple".to_string(), value(true));
        options.insert("accept_label".to_string(), value("_Send"));
        options.insert("current_folder".to_string(), value(b"/tmp\0".to_vec()));
        let images = ("Images".to_string(), vec![(0u32, "*.png".to_string()), (1, "image/*".to_string())]);
        options.insert("filters".to_string(), OwnedValue::try_from(Value::new(vec![images.clone()])).unwrap());
        options.insert("current_filter".to_string(), OwnedValue::try_from(Value::new(images)).unwrap());
        let request = common(&options);
        assert!(flag(&options, "multiple"));
        assert_eq!(request.accept_label(), "Send");
        assert_eq!(request.folder.as_deref(), Some(Path::new("/tmp")));
        assert_eq!(request.filters.len(), 1);
        assert_eq!(request.filters[0].globs, ["*.png"]);
        assert_eq!(request.filters[0].mimes, ["image/*"]);
        assert_eq!(request.current_filter, Some(0));
    }

    #[test]
    fn answers_with_uris_and_the_filter() {
        let filters = [Filter { name: "Text".into(), globs: vec!["*.txt".into()], mimes: vec![] }];
        let results = results(&[PathBuf::from("/tmp/a b.txt")], &filters, Some(0));
        let uris = <Vec<String>>::try_from(results["uris"].try_clone().unwrap()).unwrap();
        assert_eq!(uris, ["file:///tmp/a%20b.txt"]);
        let (name, rules) =
            <(String, Vec<(u32, String)>)>::try_from(results["current_filter"].try_clone().unwrap()).unwrap();
        assert_eq!((name.as_str(), rules), ("Text", vec![(0, "*.txt".to_string())]));
    }
}
