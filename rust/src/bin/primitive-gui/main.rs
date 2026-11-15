//! primitive-gui: a browser page for primitive-gpu. Starts a small web
//! server on the home network; one worker thread owns the GPU and the job.

mod export;
mod video;
mod worker;

use axum::body::{Body, Bytes};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path as UrlPath, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::sync::{oneshot, watch};
use worker::{Cmd, PicSlot, Settings, Worker};

const INDEX: &str = include_str!("../../../gui/index.html");
const APP_JS: &str = include_str!("../../../gui/app.js");
const STYLE: &str = include_str!("../../../gui/style.css");

#[derive(Clone)]
struct App {
    cmd: Arc<Mutex<mpsc::Sender<Cmd>>>,
    status: watch::Receiver<String>,
    picture: watch::Receiver<Arc<Vec<u8>>>,
    original: watch::Receiver<Arc<Vec<u8>>>,
    tmp: PathBuf,
    renders: PathBuf,
    uploads: Arc<AtomicU64>,
    /// How many pages have their live connection open.
    pages: Arc<AtomicUsize>,
}

/// The program stops by itself after this long with no page open and
/// nothing to do, so it does not keep running unseen after the tab closes.
const IDLE_STOP: std::time::Duration = std::time::Duration::from_secs(10 * 60);

impl App {
    fn send(&self, c: Cmd) {
        let _ = self.cmd.lock().unwrap().send(c);
    }
}

fn usage() -> ! {
    eprintln!(
        "Usage: primitive-gui [-port N] [-j N] [-no-open]

  -port <n>   port to listen on (default 8640)
  -j <n>      CPU threads (default: all cores)
  -no-open    do not open the page in a browser

Open http://<this computer>:<port> in a browser."
    );
    std::process::exit(1)
}

fn main() {
    let mut port: u16 = 8640;
    let mut threads: usize = 0;
    let mut open = true;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        if args[i].trim_start_matches('-') == "no-open" {
            open = false;
            i += 1;
            continue;
        }
        let next = |i: usize| args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or_else(|| usage());
        match args[i].trim_start_matches('-') {
            "port" => port = next(i) as u16,
            "j" => threads = next(i),
            _ => usage(),
        }
        i += 2;
    }
    if threads > 0 {
        let _ = rayon::ThreadPoolBuilder::new().num_threads(threads).build_global();
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio").block_on(serve(port, open));
}

/// Opens the page in the default browser, but only when someone sits at
/// this computer (not over SSH, and on Linux only with a desktop).
fn open_browser(url: &str) {
    if std::env::var_os("SSH_CONNECTION").is_some() {
        return;
    }
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(url).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let r = {
        if std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return;
        }
        std::process::Command::new("xdg-open").arg(url).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn()
    };
    if r.is_err() {
        println!("Could not open a browser. Open {url} yourself.");
    }
}

/// This computer's address on the home network. Connecting a UDP socket
/// sends nothing; it only makes the system pick the outgoing address.
fn lan_address() -> Option<std::net::IpAddr> {
    let s = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    s.connect(("192.168.0.1", 9)).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// Where renders are saved: the user's Videos folder (Movies on a Mac).
fn renders_folder() -> PathBuf {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(if cfg!(target_os = "macos") { "Movies" } else { "Videos" }).join("primitive")
}

/// A Mac marks every file from a downloaded zip as "from the internet" and
/// refuses to run the bundled ffmpeg and ffprobe. The user has already
/// allowed this program, so it clears the mark on its two helpers.
#[cfg(target_os = "macos")]
fn allow_helpers() {
    let Some(dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) else { return };
    for name in ["ffmpeg", "ffprobe"] {
        let p = dir.join(name);
        if p.is_file() {
            let _ = std::process::Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(&p).stderr(std::process::Stdio::null()).status();
        }
    }
}

async fn serve(port: u16, open: bool) {
    #[cfg(target_os = "macos")]
    allow_helpers();
    let url = format!("http://localhost:{port}");
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!("Port {port} is in use: primitive-gui is probably already running. Open {url}");
            if open {
                open_browser(&url);
            }
            std::process::exit(3);
        }
        Err(e) => {
            eprintln!("Could not listen on port {port}: {e}");
            std::process::exit(1);
        }
    };
    // One fixed temp folder: anything in it is left over from a run that
    // crashed, because only one copy can hold the port.
    let tmp = std::env::temp_dir().join("primitive-gui");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("could not make the temp folder");
    let renders = renders_folder();
    let renders_text = renders.display().to_string();

    let (cmd_tx, cmd_rx) = mpsc::channel();
    let (status_tx, status_rx) = watch::channel(String::from("{}"));
    let (pic_tx, pic_rx) = watch::channel(Arc::new(Vec::new()));
    let (orig_tx, orig_rx) = watch::channel(Arc::new(Vec::new()));
    let status_tx = Arc::new(status_tx);
    let slot = Arc::new(PicSlot::default());
    {
        let slot = slot.clone();
        std::thread::spawn(move || worker::encoder(slot, pic_tx));
    }
    let busy = Arc::new(AtomicBool::new(false));
    let worker_done = {
        let (tmp, renders, status_tx, busy) = (tmp.clone(), renders.clone(), status_tx.clone(), busy.clone());
        std::thread::spawn(move || {
            let st = status_tx.clone();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                Worker::new(cmd_rx, tmp, renders, st, orig_tx, slot, busy).run();
            }));
            if let Err(e) = r {
                let msg = e
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_else(|| "unknown error".into());
                let status = serde_json::json!({
                    "error": format!("The worker stopped with an error: {msg}. Restart primitive-gui."),
                    "settings": Settings::default(),
                });
                status_tx.send_replace(status.to_string());
            }
        })
    };

    let app = App {
        cmd: Arc::new(Mutex::new(cmd_tx)),
        status: status_rx,
        picture: pic_rx,
        original: orig_rx,
        tmp: tmp.clone(),
        renders,
        uploads: Arc::new(AtomicU64::new(0)),
        pages: Arc::new(AtomicUsize::new(0)),
    };
    let router = Router::new()
        .route("/", get(|| async { ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX) }))
        .route("/app.js", get(|| async { ([(header::CONTENT_TYPE, "text/javascript; charset=utf-8")], APP_JS) }))
        .route("/style.css", get(|| async { ([(header::CONTENT_TYPE, "text/css; charset=utf-8")], STYLE) }))
        .route("/upload", post(upload).layer(DefaultBodyLimit::disable()))
        .route("/ws", get(ws))
        .route("/original", get(original))
        .route("/export", get(export))
        .route("/download/{name}", get(download))
        .route("/sequence", post(sequence))
        .route("/zip/{name}", get(zip_download))
        .with_state(app.clone());

    println!("primitive-gui is running. Open one of these in a browser:");
    println!("  {url}");
    if let Some(ip) = lan_address() {
        println!("  http://{ip}:{port}   (phone or laptop)");
    }
    println!("Renders are saved in {renders_text}");
    println!("To stop it, close this window or press Ctrl+C.");
    println!("It also stops by itself {} minutes after the last browser tab closes, unless it is still working.", IDLE_STOP.as_secs() / 60);
    if open {
        open_browser(&url);
    }

    // Stop on Ctrl+C, when the launcher or a logout ends the program, or
    // after IDLE_STOP with no page open and nothing to do. Open pages never
    // close their live connection, so this does not wait for them: it stops
    // the worker (which stops ffmpeg), removes the temp files and exits.
    let pages = app.pages.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = stop_signal() => {}
            _ = idle_stop(pages, busy) => println!("No browser tab open for {} minutes.", IDLE_STOP.as_secs() / 60),
        }
        println!("Stopping...");
        app.send(Cmd::Quit);
        let join = tokio::task::spawn_blocking(move || worker_done.join());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), join).await;
        let _ = std::fs::remove_dir_all(&tmp);
        std::process::exit(0);
    });
    let _ = axum::serve(listener, router).await;
}

/// Returns once no page has been open and the worker has been idle for
/// `IDLE_STOP` in a row.
async fn idle_stop(pages: Arc<AtomicUsize>, busy: Arc<AtomicBool>) {
    let mut idle_since = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        if pages.load(Ordering::Relaxed) > 0 || busy.load(Ordering::Relaxed) {
            idle_since = tokio::time::Instant::now();
        } else if idle_since.elapsed() >= IDLE_STOP {
            return;
        }
    }
}

/// Waits for Ctrl+C, or for the system to end the program (logout, the
/// launcher stopping, or the console window closing on Windows).
#[cfg(unix)]
async fn stop_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).expect("signal");
    let mut hup = signal(SignalKind::hangup()).expect("signal");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        _ = hup.recv() => {}
    }
}

#[cfg(windows)]
async fn stop_signal() {
    use tokio::signal::windows::{ctrl_close, ctrl_shutdown};
    let mut close = ctrl_close().expect("signal");
    let mut shutdown = ctrl_shutdown().expect("signal");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = close.recv() => {}
        _ = shutdown.recv() => {}
    }
}

#[derive(Deserialize)]
struct UploadQuery {
    name: String,
    /// Image sequence: the upload's id and this picture's place in the order.
    seq: Option<String>,
    index: Option<u32>,
}

/// An id made by the page: letters and digits only.
fn seq_id(id: &str) -> Option<&str> {
    (!id.is_empty() && id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric())).then_some(id)
}

/// The file comes as the raw request body and goes straight to a temp file.
async fn upload(State(app): State<App>, Query(q): Query<UploadQuery>, body: Body) -> Response {
    let name: String = std::path::Path::new(&q.name).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let ext = std::path::Path::new(&name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let ext: String = ext.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    let n = app.uploads.fetch_add(1, Ordering::Relaxed);
    let seq = match q.seq.as_deref() {
        Some(id) => match (seq_id(id), q.index) {
            (Some(id), Some(i)) => Some((id.to_string(), i)),
            _ => return (StatusCode::BAD_REQUEST, "bad sequence upload").into_response(),
        },
        None => None,
    };
    let path = match &seq {
        // Zero-padded place in the order, so the names sort right.
        Some((id, i)) => {
            let dir = app.tmp.join(format!("seq-{id}"));
            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                return (StatusCode::INTERNAL_SERVER_ERROR, format!("upload failed: {e}")).into_response();
            }
            dir.join(format!("{i:08}.{ext}"))
        }
        None => app.tmp.join(format!("upload-{n}.{ext}")),
    };
    let res: Result<(), String> = async {
        let mut f = tokio::fs::File::create(&path).await.map_err(|e| e.to_string())?;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            f.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
        f.flush().await.map_err(|e| e.to_string())
    }
    .await;
    match res {
        Ok(()) => {
            if seq.is_none() {
                app.send(Cmd::Load { path, name });
            }
            StatusCode::OK.into_response()
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            (StatusCode::INTERNAL_SERVER_ERROR, format!("upload failed: {e}")).into_response()
        }
    }
}

#[derive(Deserialize)]
struct SequenceQuery {
    id: String,
    name: String,
}

/// All pictures of a sequence are uploaded: load them as frames.
async fn sequence(State(app): State<App>, Query(q): Query<SequenceQuery>) -> Response {
    let Some(id) = seq_id(&q.id) else { return (StatusCode::BAD_REQUEST, "bad sequence id").into_response() };
    let dir = app.tmp.join(format!("seq-{id}"));
    if !dir.is_dir() {
        return (StatusCode::BAD_REQUEST, "no pictures were uploaded").into_response();
    }
    app.send(Cmd::LoadSequence { dir, name: q.name });
    StatusCode::OK.into_response()
}

/// A saved image sequence folder as one zip file. The zip is made once in
/// the temp folder and kept until the program stops.
async fn zip_download(State(app): State<App>, UrlPath(name): UrlPath<String>) -> Response {
    if name.contains('/') || name.contains("..") || name.starts_with('.') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let dir = app.renders.join(&name);
    if !dir.is_dir() {
        return StatusCode::NOT_FOUND.into_response();
    }
    let out = app.tmp.join(format!("zip-{name}.zip"));
    if !out.exists() {
        let (d, o) = (dir.clone(), out.clone());
        match tokio::task::spawn_blocking(move || export::zip_folder(&d, &o)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("zip failed: {e}")).into_response(),
            Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        }
    }
    send_file(&out, &format!("{name}.zip"), "application/zip").await
}

async fn send_file(path: &std::path::Path, name: &str, mime: &str) -> Response {
    let Ok(f) = tokio::fs::File::open(path).await else { return StatusCode::NOT_FOUND.into_response() };
    let len = f.metadata().await.map(|m| m.len()).unwrap_or(0);
    let body = Body::from_stream(tokio_util::io::ReaderStream::new(f));
    (
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CONTENT_LENGTH, len.to_string()),
            (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", name.replace('"', "'"))),
        ],
        body,
    )
        .into_response()
}

async fn ws(State(app): State<App>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| live(app, socket))
}

/// One page: commands in; status and the newest picture out. A slow page
/// only ever gets the newest picture, so the worker never waits for it.
async fn live(app: App, socket: WebSocket) {
    // Counts this page as open until the function returns, however it returns.
    struct Open(Arc<AtomicUsize>);
    impl Drop for Open {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }
    app.pages.fetch_add(1, Ordering::Relaxed);
    let _open = Open(app.pages.clone());
    let (mut tx, mut rx) = socket.split();
    let mut status = app.status.clone();
    let mut picture = app.picture.clone();
    let first = status.borrow_and_update().clone();
    if tx.send(Message::Text(first.into())).await.is_err() {
        return;
    }
    let pic = picture.borrow_and_update().clone();
    if !pic.is_empty() && tx.send(Message::Binary(Bytes::from(pic.to_vec()))).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            msg = rx.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        if let Some(c) = parse_command(&t) {
                            app.send(c);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    _ => {}
                }
            }
            r = status.changed() => {
                if r.is_err() { return; }
                let s = status.borrow_and_update().clone();
                if tx.send(Message::Text(s.into())).await.is_err() { return; }
            }
            r = picture.changed() => {
                if r.is_err() { return; }
                let p = picture.borrow_and_update().clone();
                if tx.send(Message::Binary(Bytes::from(p.to_vec()))).await.is_err() { return; }
            }
        }
    }
}

fn parse_command(text: &str) -> Option<Cmd> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    Some(match v["t"].as_str()? {
        "settings" => Cmd::Settings(serde_json::from_value(v["s"].clone()).ok()?),
        "play" => Cmd::Play,
        "pause" => Cmd::Pause,
        "pause_build" => Cmd::PauseBuild(true),
        "resume_build" => Cmd::PauseBuild(false),
        "restart" => Cmd::Restart,
        "seek" => Cmd::Seek(v["frame"].as_u64()? as usize),
        "render" => Cmd::Render,
        "cancel" => Cmd::Cancel,
        "dismiss" => Cmd::DismissError,
        _ => return None,
    })
}

async fn original(State(app): State<App>) -> Response {
    let b = app.original.borrow().clone();
    if b.is_empty() {
        return StatusCode::NOT_FOUND.into_response();
    }
    ([(header::CONTENT_TYPE, "image/jpeg"), (header::CACHE_CONTROL, "no-store")], b.to_vec()).into_response()
}

async fn export(State(app): State<App>, Query(q): Query<HashMap<String, String>>) -> Response {
    let fmt = q.get("fmt").cloned().unwrap_or_default();
    let (tx, rx) = oneshot::channel();
    app.send(Cmd::Export(tx));
    let data = match rx.await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return (StatusCode::BAD_REQUEST, e).into_response(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "the worker is not running").into_response(),
    };
    let tmp = app.tmp.clone();
    match tokio::task::spawn_blocking(move || export::export(&data, &fmt, &tmp)).await {
        Ok(Ok((bytes, mime, name))) => (
            [
                (header::CONTENT_TYPE, mime.to_string()),
                (header::CONTENT_DISPOSITION, format!("attachment; filename=\"{}\"", name.replace('"', "'"))),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn download(State(app): State<App>, UrlPath(name): UrlPath<String>) -> Response {
    if name.contains('/') || name.contains("..") || name.starts_with('.') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mime = match name.rsplit('.').next().unwrap_or("") {
        "webm" => "video/webm",
        "gif" => "image/gif",
        _ => "video/mp4",
    };
    send_file(&app.renders.join(&name), &name, mime).await
}
