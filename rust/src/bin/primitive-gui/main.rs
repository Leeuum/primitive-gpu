//! primitive-gui: a browser page for primitive-gpu. Starts a small web
//! server on the home network; one worker thread owns the GPU and the job.

mod export;
mod recipe;
mod save;
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
    /// The folder of the current or last render (shared with the worker).
    render_dir: Arc<Mutex<PathBuf>>,
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
        "Usage: primitive-gui [-port N] [-j N] [-no-open] [-lan]

  -port <n>   port to listen on (default 8640)
  -j <n>      CPU threads (default: all cores)
  -no-open    do not open the page in a browser
  -lan        let other devices on the network (phone, laptop) use it;
              without it, only this computer can open the page

Open http://localhost:<port> in a browser."
    );
    std::process::exit(1)
}

fn main() {
    let mut port: u16 = 8640;
    let mut threads: usize = 0;
    let mut open = true;
    let mut lan = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].trim_start_matches('-') {
            "no-open" => open = false,
            "lan" => lan = true,
            _ => {
                let next = |i: usize| args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or_else(|| usage());
                match args[i].trim_start_matches('-') {
                    "port" => port = next(i) as u16,
                    "j" => threads = next(i),
                    _ => usage(),
                }
                i += 1;
            }
        }
        i += 1;
    }
    if threads > 0 {
        let _ = rayon::ThreadPoolBuilder::new().num_threads(threads).build_global();
    }
    tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio").block_on(serve(port, open, lan));
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

/// The user's home folder. The file browser shows only this folder and the
/// folders inside it.
fn home_folder() -> PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Where renders are saved: the user's Videos folder (Movies on a Mac).
fn renders_folder() -> PathBuf {
    let home = home_folder();
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

/// `lan` false: listen on this computer only (127.0.0.1). True: listen on
/// all interfaces, so other devices on the network can use the page.
async fn serve(port: u16, open: bool, lan: bool) {
    #[cfg(target_os = "macos")]
    allow_helpers();
    let url = format!("http://localhost:{port}");
    let listener = match tokio::net::TcpListener::bind((if lan { "0.0.0.0" } else { "127.0.0.1" }, port)).await {
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
    // One temp folder for each port. Only one copy can hold a port, so
    // anything in this folder is left over from a copy that crashed. Copies
    // on other ports have their own folders.
    let held_port = listener.local_addr().map_or(port, |a| a.port());
    let tmp = std::env::temp_dir().join(format!("primitive-gui-{held_port}"));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).expect("could not make the temp folder");
    let renders = renders_folder();
    let renders_text = renders.display().to_string();
    let render_dir = Arc::new(Mutex::new(renders.clone()));

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
        let (tmp, renders, render_dir, status_tx, busy) = (tmp.clone(), renders.clone(), render_dir.clone(), status_tx.clone(), busy.clone());
        std::thread::spawn(move || {
            let st = status_tx.clone();
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                Worker::new(cmd_rx, tmp, renders, render_dir, st, orig_tx, slot, busy).run();
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
        render_dir,
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
        .route("/mask", get(key_mask))
        .route("/recipe", get(get_recipe))
        .route("/defaults", get(|| async { ([(header::CONTENT_TYPE, "application/json")], serde_json::json!(Settings::default()).to_string()) }))
        .route("/export", get(export))
        .route("/download/{name}", get(download))
        .route("/sequence", post(sequence))
        .route("/zip/{name}", get(zip_download))
        .route("/browse", get(browse))
        .route("/open", post(open_file))
        .route("/save", get(save_work))
        .route("/save-open", post(save_open).layer(DefaultBodyLimit::disable()))
        .route("/mkdir", post(make_folder))
        .layer(axum::middleware::from_fn(same_site))
        .with_state(app.clone());

    println!("primitive-gui is running. Open one of these in a browser:");
    println!("  {url}");
    if lan {
        if let Some(ip) = lan_address() {
            println!("  http://{ip}:{port}   (phone or laptop)");
        }
        println!("WARNING: -lan is on. Any device on this network can use the page and see your pictures. It has no password.");
    } else {
        println!("Only this computer can open it. To use it from a phone or laptop, start it with -lan.");
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
    match body_to_file(body, &path).await {
        Ok(()) => {
            if seq.is_none() {
                app.send(Cmd::Load { path, name, origin: None });
            }
            StatusCode::OK.into_response()
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            (StatusCode::INTERNAL_SERVER_ERROR, format!("upload failed: {e}")).into_response()
        }
    }
}

/// Writes a request body to the file `path`.
async fn body_to_file(body: Body, path: &std::path::Path) -> Result<(), String> {
    let mut f = tokio::fs::File::create(path).await.map_err(|e| e.to_string())?;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| e.to_string())?;
        f.write_all(&chunk).await.map_err(|e| e.to_string())?;
    }
    f.flush().await.map_err(|e| e.to_string())
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

/// True when a name from a web address is one plain file name, so that
/// joining it to the renders folder stays inside that folder. On Windows a
/// backslash or a drive letter would replace the folder.
fn plain_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\\', ':'])
}

/// A saved image sequence folder as one zip file. The zip is made once in
/// the temp folder and kept until the program stops.
async fn zip_download(State(app): State<App>, UrlPath(name): UrlPath<String>) -> Response {
    if !plain_name(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let dir = app.render_dir.lock().unwrap().join(&name);
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

/// True for a name a website on the internet cannot own: "localhost", an IP
/// address, a name on the home network ("liams-pc" or "liams-pc.local").
/// `host` may end in ":port", and an IPv6 address comes in brackets.
fn local_host(host: &str) -> bool {
    let h = host.trim().to_ascii_lowercase();
    let h = match h.strip_prefix('[') {
        Some(rest) => return rest.split(']').next().is_some_and(|ip| ip.parse::<std::net::Ipv6Addr>().is_ok()),
        None => h.rsplit_once(':').map_or(h.as_str(), |(name, port)| if port.chars().all(|c| c.is_ascii_digit()) { name } else { "" }).to_string(),
    };
    !h.is_empty() && (h == "localhost" || h.ends_with(".localhost") || h.ends_with(".local") || !h.contains('.') || h.parse::<std::net::Ipv4Addr>().is_ok())
}

/// Turns away requests that come from a website instead of this page. A
/// website open in the user's browser can send requests to this program,
/// because the browser runs on this computer. Two checks stop it:
/// - Host: the address the request was sent to. A website that points its
///   own name at this computer ("DNS rebinding") fails it.
/// - Origin: the page that sent the request. Browsers add it to the live
///   connection and to posts from other websites, which the Host check
///   alone would let through.
async fn same_site(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let h = req.headers();
    let host_ok = h.get(header::HOST).and_then(|v| v.to_str().ok()).is_some_and(local_host);
    let origin_ok = match h.get(header::ORIGIN).map(|v| v.to_str().unwrap_or("")) {
        None => true,
        Some(o) => o.strip_prefix("http://").is_some_and(|rest| local_host(rest.split('/').next().unwrap_or(""))),
    };
    if host_ok && origin_ok {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, "Open this page from its own address (for example http://localhost:8640).").into_response()
    }
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
        "mask" => {
            let (w, h) = (v["w"].as_u64()? as usize, v["h"].as_u64()? as usize);
            let bits = v["bits"].as_str()?;
            if w > 1024 || h > 1024 || bits.len() != w * h {
                return None;
            }
            Cmd::Mask { w, h, gray: bits.bytes().map(|b| if b == b'1' { 255 } else { 0 }).collect() }
        }
        "seek" => Cmd::Seek(v["frame"].as_u64()? as usize),
        "shape_view" => Cmd::ShapeView(v["at"].as_u64()? as usize),
        "key_set" => Cmd::KeySet { field: v["field"].as_str()?.to_string(), at: v["at"].as_u64()? as usize },
        "key_del" => Cmd::KeyDelete { field: v["field"].as_str()?.to_string(), at: v["at"].as_u64()? as usize },
        "key_smooth" => Cmd::KeySmooth { field: v["field"].as_str()?.to_string(), on: v["on"].as_bool()? },
        "key_move" => Cmd::KeyMove { from: v["from"].as_u64()? as usize, to: v["to"].as_u64()? as usize },
        "recipe" if v["r"].is_object() => Cmd::Recipe(v["r"].clone()),
        "render" => Cmd::Render(match v["dir"].as_str() {
            Some(d) => Some(out_folder(d)?),
            None => None,
        }),
        "cancel" => Cmd::Cancel,
        "dismiss" => Cmd::DismissError,
        _ => return None,
    })
}

/// The painted area in use now. The page asks for it when a keyframe has
/// changed the area.
async fn key_mask(State(app): State<App>) -> Response {
    let (tx, rx) = oneshot::channel();
    app.send(Cmd::GetMask(tx));
    let (w, h, bits) = match rx.await.ok().flatten() {
        Some((w, h, gray)) => (w, h, gray.iter().map(|&b| if b >= 128 { '1' } else { '0' }).collect()),
        None => (0, 0, String::new()),
    };
    ([(header::CONTENT_TYPE, "application/json")], serde_json::json!({ "w": w, "h": h, "bits": bits }).to_string()).into_response()
}

/// The recipe of the current work (see `recipe.rs`), for Save.
async fn get_recipe(State(app): State<App>) -> Response {
    let (tx, rx) = oneshot::channel();
    app.send(Cmd::GetRecipe(tx));
    match rx.await {
        Ok(r) => ([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], serde_json::to_string_pretty(&r).unwrap_or_default()).into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "the worker is not running").into_response(),
    }
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
    // A folder picked by the user: check it before the work is done.
    if let Some(d) = q.get("dir") {
        if out_folder(d).is_none() {
            return (StatusCode::BAD_REQUEST, FOLDER_GONE).into_response();
        }
    }
    let (tx, rx) = oneshot::channel();
    app.send(Cmd::Export(tx));
    let data = match rx.await {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => return (StatusCode::BAD_REQUEST, e).into_response(),
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "the worker is not running").into_response(),
    };
    let tmp = app.tmp.clone();
    match tokio::task::spawn_blocking(move || export::export(&data, &fmt, &tmp)).await {
        Ok(Ok((bytes, _, name))) if q.contains_key("dir") => {
            let target = match place(&q["dir"], &name, q.get("replace").map(|s| s.as_str())) {
                Ok(t) => t,
                Err(r) => return r,
            };
            match tokio::fs::write(&target, bytes).await {
                Ok(()) => saved_reply(&target),
                Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Could not write {}: {e}", target.display())).into_response(),
            }
        }
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
    if !plain_name(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mime = match name.rsplit('.').next().unwrap_or("") {
        "webm" => "video/webm",
        "gif" => "image/gif",
        _ => "video/mp4",
    };
    let path = app.render_dir.lock().unwrap().join(&name);
    send_file(&path, &name, mime).await
}

// ------------------------------------------------------- output folders
//
// Save, Export and Render can write into a folder the user picked in the
// file browser instead of sending the file to the browser. Like the file
// browser, this only reaches folders inside the home folder.

const FOLDER_GONE: &str = "That folder cannot be used (it may have been moved or deleted). Pick another one.";

/// `dir` as an absolute path when it is a folder inside the home folder.
fn out_folder(dir: &str) -> Option<PathBuf> {
    let (_, p) = inside_home(std::path::Path::new(dir))?;
    p.is_dir().then_some(p)
}

/// Where a file named `name` goes in the picked folder `dir`. When a file
/// of that name is there, `replace` decides: "1" writes over it, "0" adds
/// -2, -3 to the name, and no answer sends back 409 with `{exists}` so the
/// page can ask the user.
fn place(dir: &str, name: &str, replace: Option<&str>) -> Result<PathBuf, Response> {
    let Some(dir) = out_folder(dir) else { return Err((StatusCode::BAD_REQUEST, FOLDER_GONE).into_response()) };
    if !plain_name(name) {
        return Err((StatusCode::BAD_REQUEST, format!("{name} is not a usable file name. Change File name in Export.")).into_response());
    }
    let p = dir.join(name);
    if !p.exists() {
        return Ok(p);
    }
    match replace {
        Some("1") if p.is_file() => Ok(p),
        Some("0") | Some("1") => {
            let path = std::path::Path::new(name);
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            let ext = path.extension().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            Ok(export::unique(&dir, &stem, &ext))
        }
        _ => Err((StatusCode::CONFLICT, [(header::CONTENT_TYPE, "application/json")], serde_json::json!({ "exists": name }).to_string()).into_response()),
    }
}

/// `path` the way the file browser shows it: Home/folder/file.
fn shown_path(path: &std::path::Path) -> String {
    let home = std::fs::canonicalize(home_folder()).unwrap_or_else(|_| home_folder());
    match path.strip_prefix(&home) {
        Ok(rest) => format!("Home/{}", rest.to_string_lossy().replace('\\', "/")),
        Err(_) => path.display().to_string(),
    }
}

fn saved_reply(path: &std::path::Path) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], serde_json::json!({ "saved": shown_path(path) }).to_string()).into_response()
}

#[derive(Deserialize)]
struct MkdirQuery {
    dir: String,
    name: String,
}

/// Makes a new folder inside a folder the file browser shows.
async fn make_folder(Query(q): Query<MkdirQuery>) -> Response {
    let name = q.name.trim().to_string();
    let Some(dir) = out_folder(&q.dir) else { return (StatusCode::BAD_REQUEST, FOLDER_GONE).into_response() };
    if !plain_name(&name) {
        return (StatusCode::BAD_REQUEST, "A folder name cannot start with a dot or hold / \\ or :.").into_response();
    }
    let p = dir.join(&name);
    match std::fs::create_dir(&p) {
        Ok(()) => ([(header::CONTENT_TYPE, "application/json")], serde_json::json!({ "path": p }).to_string()).into_response(),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (StatusCode::CONFLICT, format!("{name} is already there.")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Could not make the folder: {e}")).into_response(),
    }
}

// ---------------------------------------------------------- file browser
//
// The page can list the home folder and the folders inside it, and open a
// picture or video from there. It never moves or deletes a file; it only
// writes new files and folders into picked folders (see output folders
// above), and writes over a file only when the user said so. Every path from the page is made absolute with
// `canonicalize` (which follows links and removes "..") and must then lie
// inside the home folder. Hidden files and folders are not shown.

const BROWSE_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "bmp", "tif", "tiff", "tga", "heic", "heif", "avif", "qoi", "ppm", "pnm", "exr", "dpx", "gif", "mp4", "mov", "mkv",
    "webm", "avi", "m4v", "mpg", "mpeg", "wmv", "flv", "ts", "mts", "m2ts", "ogv", "3gp", "primitive",
];

/// The home folder as an absolute path, and `path` as an absolute path if it
/// lies inside it. None for anything outside the home folder.
fn inside_home(path: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
    let home = std::fs::canonicalize(home_folder()).ok()?;
    let p = std::fs::canonicalize(path).ok()?;
    p.starts_with(&home).then_some((home, p))
}

fn browse_ext_ok(p: &std::path::Path) -> bool {
    p.extension().is_some_and(|e| BROWSE_EXT.contains(&e.to_string_lossy().to_lowercase().as_str()))
}

/// Hidden: the name starts with a dot, or (Windows) the hidden mark is set.
fn hidden(name: &str, meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x2 != 0 {
            return true;
        }
    }
    let _ = meta;
    name.starts_with('.')
}

#[derive(Deserialize)]
struct BrowseQuery {
    dir: Option<String>,
}

/// One folder's folders and pictures/videos as JSON. No `dir` means the
/// home folder.
async fn browse(Query(q): Query<BrowseQuery>) -> Response {
    let r = tokio::task::spawn_blocking(move || -> Option<serde_json::Value> {
        let asked = q.dir.map(PathBuf::from).unwrap_or_else(home_folder);
        let (home, dir) = inside_home(&asked)?;
        if !dir.is_dir() {
            return None;
        }
        let (mut folders, mut files) = (Vec::new(), Vec::new());
        for e in std::fs::read_dir(&dir).ok()?.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(meta) = std::fs::metadata(e.path()) else { continue };
            if hidden(&name, &meta) {
                continue;
            }
            // A link that points outside the home folder is left out.
            let Some((_, real)) = inside_home(&e.path()) else { continue };
            if meta.is_dir() {
                folders.push(serde_json::json!({ "name": name, "path": real }));
            } else if meta.is_file() && browse_ext_ok(&real) {
                files.push(serde_json::json!({ "name": name, "path": real, "size": meta.len() }));
            }
        }
        let key = |v: &serde_json::Value| v["name"].as_str().unwrap_or("").to_lowercase();
        folders.sort_by_key(key);
        files.sort_by_key(key);
        let up = (dir != home).then(|| dir.parent().map(|p| p.to_path_buf())).flatten();
        let shown = match dir.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "Home".to_string(),
            Ok(rest) => format!("Home/{}", rest.to_string_lossy().replace('\\', "/")),
            Err(_) => return None,
        };
        Some(serde_json::json!({ "dir": dir, "shown": shown, "up": up, "folders": folders, "files": files }))
    })
    .await;
    match r {
        Ok(Some(v)) => ([(header::CONTENT_TYPE, "application/json"), (header::CACHE_CONTROL, "no-store")], v.to_string()).into_response(),
        _ => (StatusCode::NOT_FOUND, "That folder cannot be shown.").into_response(),
    }
}

#[derive(Deserialize)]
struct OpenQuery {
    path: String,
    /// For a save file: see `SaveOpenQuery`.
    source: Option<String>,
    settings: Option<String>,
}

/// Opens a file the user picked in the file browser. The worker deletes its
/// source file when it loads another one, so it never gets the user's file:
/// it gets a hard link in the temp folder (deleting a link leaves the file),
/// or a copy when a link cannot be made (another drive). A `.primitive`
/// save file is only read.
async fn open_file(State(app): State<App>, Query(q): Query<OpenQuery>) -> Response {
    let n = app.uploads.fetch_add(1, Ordering::Relaxed);
    let tmp = app.tmp.clone();
    let asked = q.path.clone();
    let r = tokio::task::spawn_blocking(move || -> Result<Result<(PathBuf, String, PathBuf), PathBuf>, (StatusCode, String)> {
        let real = home_file(&asked).ok_or_else(|| (StatusCode::NOT_FOUND, "That file cannot be opened.".to_string()))?;
        if is_save(&real) {
            return Ok(Err(real));
        }
        let name = real.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let work = link_into_tmp(&real, &tmp, n).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Could not open {name}: {e}")))?;
        Ok(Ok((work, name, real)))
    })
    .await;
    match r {
        Ok(Ok(Ok((path, name, real)))) => {
            app.send(Cmd::Load { path, name, origin: Some(real) });
            StatusCode::OK.into_response()
        }
        Ok(Ok(Err(save_file))) => open_save(app, save_file, q.source, q.settings.as_deref() == Some("1"), false).await,
        Ok(Err(e)) => e.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `path` as an absolute path, when it is a file inside the home folder
/// that the file browser shows.
fn home_file(path: &str) -> Option<PathBuf> {
    let (_, real) = inside_home(std::path::Path::new(path))?;
    (real.is_file() && browse_ext_ok(&real)).then_some(real)
}

fn is_save(p: &std::path::Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("primitive"))
}

/// A hard link to `real` in the temp folder, or a copy when a link cannot
/// be made. The worker may delete it; the user's file stays.
fn link_into_tmp(real: &std::path::Path, tmp: &std::path::Path, n: u64) -> Result<PathBuf, String> {
    let ext = real.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let work = tmp.join(format!("upload-{n}.{ext}"));
    if std::fs::hard_link(real, &work).is_err() {
        std::fs::copy(real, &work).map_err(|e| {
            let _ = std::fs::remove_file(&work);
            e.to_string()
        })?;
    }
    Ok(work)
}

// ------------------------------------------------------------ save files
//
// See `save.rs`. Save gives a `.primitive` file to download. Opening one
// loads its source: a file the user picked to replace a missing one, else
// the file where it was at the save, else the picture inside the save
// file. When none of these is there, the page is told the source is
// missing and asks the user to find it.

#[derive(Deserialize)]
struct SaveQuery {
    /// "1": put a copy of the still picture into the save file.
    pic: Option<String>,
    /// A folder picked by the user: the file is written there instead of
    /// being sent to the browser. See `place` for `replace`.
    dir: Option<String>,
    replace: Option<String>,
}

async fn save_work(State(app): State<App>, Query(q): Query<SaveQuery>) -> Response {
    if q.dir.as_deref().is_some_and(|d| out_folder(d).is_none()) {
        return (StatusCode::BAD_REQUEST, FOLDER_GONE).into_response();
    }
    let (tx, rx) = oneshot::channel();
    app.send(Cmd::GetSave(tx));
    let Ok(data) = rx.await else { return (StatusCode::INTERNAL_SERVER_ERROR, "the worker is not running").into_response() };
    let n = app.uploads.fetch_add(1, Ordering::Relaxed);
    let out = app.tmp.join(format!("save-{n}.primitive"));
    let stem = std::path::Path::new(&data.name).file_stem().map(|s| s.to_string_lossy().to_string()).filter(|s| !s.is_empty());
    let name = format!("{}.primitive", stem.unwrap_or_else(|| "primitive-save".into()));
    let target = match &q.dir {
        Some(d) => match place(d, &name, q.replace.as_deref()) {
            Ok(t) => Some(t),
            Err(r) => return r,
        },
        None => None,
    };
    let include = q.pic.as_deref() == Some("1");
    let o = out.clone();
    match tokio::task::spawn_blocking(move || save::write(&data, include, &o)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return (StatusCode::INTERNAL_SERVER_ERROR, format!("Save failed: {e}")).into_response(),
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
    if let Some(target) = target {
        let r = match tokio::fs::copy(&out, &target).await {
            Ok(_) => saved_reply(&target),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("Could not write {}: {e}", target.display())).into_response(),
        };
        let _ = tokio::fs::remove_file(&out).await;
        return r;
    }
    let r = send_file(&out, &name, "application/zip").await;
    // The open file stays readable on Linux and macOS while it is sent.
    // On Windows the delete fails and the temp folder is cleared at exit.
    let _ = tokio::fs::remove_file(&out).await;
    r
}

#[derive(Deserialize)]
struct SaveOpenQuery {
    /// A file in the home folder picked by the user to use instead of the
    /// saved source (it has moved).
    source: Option<String>,
    /// "1": use only the recipe (the user gave up on finding the source).
    settings: Option<String>,
}

/// A save file uploaded from the page.
async fn save_open(State(app): State<App>, Query(q): Query<SaveOpenQuery>, body: Body) -> Response {
    let n = app.uploads.fetch_add(1, Ordering::Relaxed);
    let path = app.tmp.join(format!("open-{n}.primitive"));
    if let Err(e) = body_to_file(body, &path).await {
        let _ = tokio::fs::remove_file(&path).await;
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("upload failed: {e}")).into_response();
    }
    open_save(app, path, q.source, q.settings.as_deref() == Some("1"), true).await
}

/// Opens the save file `file`. `settings_only`: use only its recipe.
/// `uploaded`: it is a temp copy to delete.
/// Answers JSON: `{ "ok": true }`, or `{ "missing": name, "kind": .. }`
/// when the source has to be found, or `{ "ok": true, "settings_only":
/// kind }` when the save has no source this program can open again.
async fn open_save(app: App, file: PathBuf, source: Option<String>, settings_only: bool, uploaded: bool) -> Response {
    let n = app.uploads.fetch_add(1, Ordering::Relaxed);
    let tmp = app.tmp.clone();
    let f = file.clone();
    let r = tokio::task::spawn_blocking(move || -> Result<(serde_json::Value, Option<Cmd>), (StatusCode, String)> {
        let bad = |e: String| (StatusCode::BAD_REQUEST, e);
        let s = save::read(&f).map_err(bad)?;
        if settings_only || !matches!(s.kind.as_str(), "image" | "video") {
            let kind = s.kind.clone();
            return Ok((serde_json::json!({ "ok": true, "settings_only": kind }), Some(Cmd::Recipe(s.recipe))));
        }
        // A picked file, else the saved place. Both must pass the same
        // checks as the file browser: a save file could name any path.
        let real = match &source {
            Some(p) => Some(home_file(p).filter(|p| !is_save(p)).ok_or_else(|| bad("That file cannot be opened.".into()))?),
            None => s.path.as_ref().and_then(|p| home_file(&p.to_string_lossy())).filter(|p| !is_save(p)),
        };
        let (work, name, origin) = match (real, &s.included) {
            (Some(real), _) => {
                let name = real.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let work = link_into_tmp(&real, &tmp, n).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Could not open {name}: {e}")))?;
                (work, name, Some(real))
            }
            (None, Some(inner)) => {
                let ext = std::path::Path::new(inner).extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
                let work = tmp.join(format!("upload-{n}.{ext}"));
                save::extract(&f, inner, &work).map_err(bad)?;
                (work, s.name.clone(), None)
            }
            (None, None) => return Ok((serde_json::json!({ "missing": s.name, "kind": s.kind }), None)),
        };
        let cmd = Cmd::LoadSave { path: work, name, origin, recipe: s.recipe, shapes: s.shapes };
        Ok((serde_json::json!({ "ok": true }), Some(cmd)))
    })
    .await;
    if uploaded {
        let _ = tokio::fs::remove_file(&file).await;
    }
    match r {
        Ok(Ok((answer, cmd))) => {
            if let Some(cmd) = cmd {
                app.send(cmd);
            }
            ([(header::CONTENT_TYPE, "application/json")], answer.to_string()).into_response()
        }
        Ok(Err(e)) => e.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
