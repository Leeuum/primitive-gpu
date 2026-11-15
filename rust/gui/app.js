// primitive-gui page. All job state lives in the program; this page shows
// what the program sends and sends commands back.
"use strict";

const $ = (id) => document.getElementById(id);

// ------------------------------------------------------------- popups
// Styled stand-ins for confirm() and alert(). A warning the user marked
// "Don't ask again" is remembered in this browser under its key and then
// answers yes by itself. "Show skipped warnings again" forgets them all.
const SKIP = "primitive-skip-";
function skipped() { return Object.keys(localStorage).filter((k) => k.startsWith(SKIP)); }
function popup(text, { key = null, ok = "Go on", cancel = "Cancel" } = {}) {
  const d = $("ask");
  $("ask-text").textContent = text;
  $("ask-yes").textContent = ok;
  $("ask-no").textContent = cancel;
  $("ask-no").hidden = !cancel;
  $("ask-skip-row").hidden = !key;
  $("ask-skip").checked = false;
  d.returnValue = "";
  d.showModal();
  $("ask-yes").focus();
  return new Promise((done) => {
    d.addEventListener("close", () => {
      const yes = d.returnValue === "yes";
      if (yes && key && $("ask-skip").checked) { localStorage.setItem(SKIP + key, "1"); showAskReset(); }
      done(yes);
    }, { once: true });
  });
}
async function ask(key, text) {
  if (localStorage.getItem(SKIP + key)) return true;
  return popup(text, { key });
}
function tell(text) { return popup(text, { ok: "OK", cancel: null }); }
function showAskReset() { $("ask-reset").hidden = skipped().length === 0; }
$("ask-reset").onclick = () => { for (const k of skipped()) localStorage.removeItem(k); showAskReset(); };
showAskReset();
let ws = null;
let st = null;               // newest status from the program
let picUrl = null;
let myRender = false;        // this page pressed Render
let downloaded = null;       // render file this page already downloaded
let lastEdit = 0;            // time of the last settings edit on this page
let connected = false;
let localError = null;

// ------------------------------------------------------------ connection

function connect() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  ws = new WebSocket(`${proto}://${location.host}/ws`);
  ws.binaryType = "blob";
  ws.onopen = () => { connected = true; localError = null; render(); };
  ws.onclose = () => {
    connected = false;
    localError = "Lost the connection to primitive-gui. Trying again…";
    render();
    setTimeout(connect, 1500);
  };
  ws.onmessage = (ev) => {
    if (typeof ev.data === "string") {
      st = JSON.parse(ev.data);
      render();
    } else {
      const url = URL.createObjectURL(ev.data);
      $("picture").onload = () => { if (picUrl && picUrl !== url) URL.revokeObjectURL(picUrl); picUrl = url; };
      $("picture").src = url;
    }
  };
}

function send(obj) {
  if (ws && ws.readyState === 1) ws.send(JSON.stringify(obj));
}

// -------------------------------------------------------------- settings

const NUM = ["count", "min_count", "mode", "alpha", "work_size", "out_w", "out_h", "repeat", "chains", "random", "age", "k", "frame_digits", "frame_start"];
const FLOAT = ["seq_fps"];
const TEXT = ["background", "seed", "metric", "out_preset", "export_fmt", "name_pattern"];
const ALL = NUM.concat(FLOAT, TEXT);

const FORMATS = {
  image: [["png", "PNG picture"], ["jpg", "JPG picture"], ["svg", "SVG picture"],
          ["mp4", "Timelapse MP4"], ["webm", "Timelapse WebM"], ["gif", "Timelapse GIF"]],
  video: [["mp4", "MP4 video"], ["webm", "WebM video"], ["gif", "GIF animation"],
          ["png-seq", "PNG image sequence"], ["jpg-seq", "JPG image sequence"], ["svg-seq", "SVG image sequence"]],
};
let formatKind = null;
function setFormatOptions(kind) {
  if (kind === formatKind) return;
  formatKind = kind;
  const sel = $("export_fmt");
  sel.innerHTML = "";
  for (const [v, label] of FORMATS[kind]) {
    const o = document.createElement("option");
    o.value = v; o.textContent = label;
    sel.append(o);
  }
}

function readSettings() {
  const s = {};
  for (const k of NUM) {
    const v = parseInt($(k).value, 10);
    s[k] = Number.isFinite(v) ? v : (st && st.settings ? st.settings[k] : 0);
  }
  for (const k of FLOAT) {
    const v = parseFloat($(k).value);
    s[k] = Number.isFinite(v) ? v : (st && st.settings ? st.settings[k] : 0);
  }
  for (const k of TEXT) s[k] = $(k).value.trim();
  const ts = parseFloat($("target_score").value);
  s.target_score = Number.isFinite(ts) && ts > 0 ? ts : 0;
  s.build_on = document.querySelector('input[name=frames][value=build]').checked;
  s.realtime = $("realtime").checked;
  return s;
}

function sendSettings() {
  lastEdit = Date.now();
  send({ t: "settings", s: readSettings() });
}

function fillSettings(s) {
  // Do not overwrite a field while someone is typing in it on this page.
  const typing = Date.now() - lastEdit < 1500;
  for (const k of ALL) {
    const el = $(k);
    if (el === document.activeElement || typing) continue;
    if (String(el.value) !== String(s[k])) el.value = s[k];
  }
  const tsEl = $("target_score");
  if (tsEl !== document.activeElement && !typing) {
    const v = s.target_score > 0 ? String(s.target_score) : "";
    if (tsEl.value !== v) tsEl.value = v;
  }
  if (!typing) {
    $("alpha-range").value = s.alpha;
    $("work-range").value = s.work_size;
    document.querySelector(`input[name=frames][value=${s.build_on ? "build" : "fresh"}]`).checked = true;
    $("realtime").checked = !!s.realtime;
    if (/^#?[0-9a-fA-F]{6}$/.test(s.background)) $("background-pick").value = "#" + s.background.replace("#", "");
  }
}

for (const k of ALL) {
  $(k).addEventListener("change", sendSettings);
}

// Save the settings to a .json file, and load them back. A loaded file only
// changes the settings it names, so files from older versions still work.
$("settings-save").onclick = () => {
  const data = { app: "primitive-gui", settings: readSettings() };
  const blob = new Blob([JSON.stringify(data, null, 2)], { type: "application/json" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "primitive-settings.json";
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 1000);
};
$("settings-load").onclick = () => $("settings-file").click();
$("settings-file").addEventListener("change", async () => {
  const f = $("settings-file").files[0];
  $("settings-file").value = "";
  if (!f) return;
  if (st && st.render) { tell("A render is running. Cancel it first."); return; }
  let loaded;
  try {
    const data = JSON.parse(await f.text());
    loaded = data && typeof data.settings === "object" ? data.settings : data;
  } catch (e) {
    tell(`${f.name} is not a settings file.`);
    return;
  }
  const cur = readSettings();
  const known = Object.keys(cur).filter((k) => loaded && k in loaded && typeof loaded[k] === typeof cur[k]);
  if (!known.length) { tell(`${f.name} has no settings in it.`); return; }
  const next = { ...cur };
  for (const k of known) next[k] = loaded[k];
  // A file type that this kind of source does not offer keeps the current one.
  if (![...$("export_fmt").options].some((o) => o.value === next.export_fmt)) next.export_fmt = cur.export_fmt;
  if (next.build_on !== cur.build_on && st && st.has_work &&
      !(await ask("switch-frames", "These settings switch how video frames are built. That clears the shapes of every frame. Go on?"))) return;
  lastEdit = 0;
  fillSettings(next);
  sendSettings();
});
$("target_score").addEventListener("change", sendSettings);
$("realtime").addEventListener("change", sendSettings);
// Typing a name pattern updates the example name shortly after.
let patternTimer = null;
$("name_pattern").addEventListener("input", () => {
  lastEdit = Date.now();
  clearTimeout(patternTimer);
  patternTimer = setTimeout(sendSettings, 300);
});
$("mode").addEventListener("input", sendSettings);
$("alpha-range").addEventListener("input", () => { $("alpha").value = $("alpha-range").value; sendSettings(); });
$("work-range").addEventListener("input", () => { $("work_size").value = $("work-range").value; sendSettings(); });
$("background-pick").addEventListener("input", () => { $("background").value = $("background-pick").value; sendSettings(); });
for (const r of document.querySelectorAll("input[name=frames]")) {
  r.addEventListener("change", async () => {
    if (st && st.has_work && !(await ask("switch-frames", "Switching this clears the shapes of every frame. Go on?"))) {
      fillSettings(st.settings);
      lastEdit = 0;
      fillSettings(st.settings);
      return;
    }
    sendSettings();
  });
}

// ---------------------------------------------------------------- upload

async function upload(file) {
  if (!file) return;
  if (st && st.render) { tell("A render is running. Cancel it first."); return; }
  if (st && st.has_work && st.kind === "video" && !(await ask("load-file", "Loading a new file throws away the work on this video. Go on?"))) return;
  const xhr = new XMLHttpRequest();
  xhr.open("POST", "/upload?name=" + encodeURIComponent(file.name));
  xhr.upload.onprogress = (e) => {
    if (e.lengthComputable) showBusy(`Uploading ${file.name}`, e.loaded / e.total);
  };
  xhr.onload = () => {
    hideBusy();
    if (xhr.status !== 200) { localError = xhr.responseText || "Upload failed."; render(); }
  };
  xhr.onerror = () => { hideBusy(); localError = "Upload failed."; render(); };
  showBusy(`Uploading ${file.name}`, 0);
  xhr.send(file);
}

let uploading = false;
function showBusy(text, p) {
  uploading = true;
  $("busy").hidden = false;
  $("busy-text").textContent = text;
  $("busy-bar").style.width = `${Math.round((p || 0) * 100)}%`;
}
function hideBusy() { uploading = false; render(); }

// Image sequences: several pictures, or a folder of them.
const PICTURE = /\.(png|jpe?g|webp|bmp|tiff?|tga|heic|heif|avif|qoi|ppm|pnm|exr|dpx)$/i;

// Files the user picked or dropped, as { file, path } with the path inside
// a dropped folder.
function uploadMany(items) {
  if (!items.length) return;
  if (items.length === 1) return upload(items[0].file);
  const pics = items.filter((it) => PICTURE.test(it.file.name));
  if (pics.length >= 2) return uploadSequence(pics);
  if (pics.length === 1) return upload(pics[0].file);
  localError = "Drop one video, or two or more pictures for an image sequence.";
  render();
}

function sequenceName(pics) {
  // "owl_0001.png" gives "owl". Falls back to the folder name.
  const stem = pics[0].file.name.replace(/\.[^.]*$/, "").replace(/[\d_\-. ]+$/, "");
  if (stem) return stem;
  const parts = pics[0].path.split("/");
  return parts.length > 1 ? parts[parts.length - 2] : "sequence";
}

async function uploadSequence(pics) {
  if (st && st.render) { tell("A render is running. Cancel it first."); return; }
  if (st && st.has_work && st.kind === "video" && !(await ask("load-pictures", "Loading new pictures throws away the work on this video. Go on?"))) return;
  // Order by name, with numbers compared as numbers: 2 comes before 10.
  pics.sort((a, b) => a.path.localeCompare(b.path, undefined, { numeric: true, sensitivity: "base" }));
  const id = Array.from(crypto.getRandomValues(new Uint8Array(8)), (b) => b.toString(16).padStart(2, "0")).join("");
  const name = sequenceName(pics);
  const total = pics.reduce((n, it) => n + it.file.size, 0) || 1;
  let sent = 0, next = 0, failed = null;
  const label = `Uploading ${pics.length} pictures`;
  showBusy(label, 0);
  async function worker() {
    while (next < pics.length && !failed) {
      const i = next++;
      const f = pics[i].file;
      try {
        const r = await fetch(`/upload?name=${encodeURIComponent(f.name)}&seq=${id}&index=${i}`, { method: "POST", body: f });
        if (!r.ok) throw new Error(await r.text());
      } catch (e) {
        failed = e.message || "Upload failed.";
      }
      sent += f.size;
      showBusy(label, sent / total);
    }
  }
  await Promise.all([worker(), worker(), worker(), worker()]);
  if (!failed) {
    const r = await fetch(`/sequence?id=${id}&name=${encodeURIComponent(name)}`, { method: "POST" }).catch((e) => ({ ok: false, text: async () => e.message }));
    if (!r.ok) failed = await r.text();
  }
  hideBusy();
  if (failed) { localError = `Upload failed: ${failed}`; render(); }
}

// Reads a dropped folder, and the folders inside it.
async function readEntry(entry, out) {
  if (entry.isFile) {
    const file = await new Promise((ok, bad) => entry.file(ok, bad));
    out.push({ file, path: entry.fullPath.replace(/^\//, "") });
  } else if (entry.isDirectory) {
    const reader = entry.createReader();
    for (;;) {
      const batch = await new Promise((ok, bad) => reader.readEntries(ok, bad));
      if (!batch.length) break;
      for (const e of batch) await readEntry(e, out);
    }
  }
}

$("pick").onclick = () => $("file").click();
$("pick-folder").onclick = () => $("folder").click();
$("open").onclick = () => $("file").click();
const fromInput = (input) => Array.from(input.files, (file) => ({ file, path: file.webkitRelativePath || file.name }));
$("file").onchange = () => { uploadMany(fromInput($("file"))); $("file").value = ""; };
$("folder").onchange = () => { uploadMany(fromInput($("folder"))); $("folder").value = ""; };
const view = $("view");
document.addEventListener("dragover", (e) => { e.preventDefault(); view.classList.add("dragover"); });
document.addEventListener("dragleave", (e) => { if (!e.relatedTarget) view.classList.remove("dragover"); });
document.addEventListener("drop", async (e) => {
  e.preventDefault();
  view.classList.remove("dragover");
  const entries = Array.from(e.dataTransfer.items || [], (it) => it.webkitGetAsEntry && it.webkitGetAsEntry()).filter(Boolean);
  const plain = Array.from(e.dataTransfer.files, (file) => ({ file, path: file.name }));
  if (!entries.length || !entries.some((en) => en.isDirectory)) return uploadMany(plain);
  const out = [];
  try {
    for (const en of entries) await readEntry(en, out);
  } catch (err) {
    localError = `Could not read the folder: ${err.message || err}`;
    return render();
  }
  uploadMany(out);
});

// --------------------------------------------------------------- buttons

$("play").onclick = () => send({ t: st && st.playing ? "pause" : "play" });

$("pause-build").onclick = () => send({ t: st && st.build_paused ? "resume_build" : "pause_build" });

$("restart").onclick = async () => {
  if (st && st.has_work && st.kind === "video" && !(await ask("restart", "Restart clears the shapes of every frame. Go on?"))) return;
  send({ t: "restart" });
};

$("render").onclick = async () => {
  if (!st) return;
  if (st.render) { send({ t: "cancel" }); myRender = false; return; }
  if (st.kind === "video") { myRender = true; send({ t: "render" }); return; }
  const fmt = $("export_fmt").value;
  $("render").disabled = true;
  $("render").textContent = ["mp4", "webm", "gif"].includes(fmt) ? "Making video…" : "Exporting…";
  try {
    const r = await fetch(`/export?fmt=${fmt}`);
    if (!r.ok) throw new Error(await r.text());
    const blob = await r.blob();
    const cd = r.headers.get("content-disposition") || "";
    const m = cd.match(/filename="([^"]+)"/);
    saveBlob(blob, m ? m[1] : `picture.${fmt}`);
  } catch (e) {
    localError = `Export failed: ${e.message}`;
  }
  $("render").disabled = false;
  render();
};

function saveBlob(blob, name) {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = name;
  document.body.appendChild(a);
  a.click();
  setTimeout(() => { URL.revokeObjectURL(a.href); a.remove(); }, 1000);
}

$("error-close").onclick = () => { localError = null; send({ t: "dismiss" }); render(); };

// Hold to see the original picture.
const hold = $("hold-original");
function showOriginal(on) {
  if (!st || !st.ready) return;
  const o = $("original");
  if (on) { o.src = `/original?v=${st.original}`; o.hidden = false; } else { o.hidden = true; }
}
hold.addEventListener("pointerdown", (e) => { e.preventDefault(); hold.setPointerCapture(e.pointerId); showOriginal(true); });
hold.addEventListener("pointerup", () => showOriginal(false));
hold.addEventListener("pointercancel", () => showOriginal(false));
hold.addEventListener("contextmenu", (e) => e.preventDefault());

// -------------------------------------------------------------- timeline

const tl = $("timeline");
let seeking = false;
let lastSeek = 0;
function seekAt(e) {
  if (!st || !st.video || st.render) return;
  const r = tl.getBoundingClientRect();
  const n = st.video.frames;
  const f = Math.max(0, Math.min(n - 1, Math.floor((e.clientX - r.left) / r.width * n)));
  const now = Date.now();
  if (now - lastSeek < 40 && e.type === "pointermove") return;
  lastSeek = now;
  send({ t: "seek", frame: f });
}
tl.addEventListener("pointerdown", (e) => { seeking = true; tl.setPointerCapture(e.pointerId); seekAt(e); });
tl.addEventListener("pointermove", (e) => { if (seeking) seekAt(e); });
tl.addEventListener("pointerup", (e) => { seeking = false; seekAt(e); });

function drawTimeline() {
  const v = st.video;
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(tl.clientWidth * dpr)), h = Math.round(34 * dpr);
  if (tl.width !== w || tl.height !== h) { tl.width = w; tl.height = h; }
  const g = tl.getContext("2d");
  g.fillStyle = "#1a1a1a";
  g.fillRect(0, 0, w, h);
  const n = v.frames, line = v.timeline;
  const colors = { "0": "#3a3a3a", "1": "#c9a227", "2": "#3fa34d" };
  const barTop = Math.round(8 * dpr), barH = h - barTop * 2;
  if (n <= w) {
    for (let i = 0; i < n; i++) {
      g.fillStyle = colors[line[i]];
      const x0 = Math.floor(i * w / n), x1 = Math.floor((i + 1) * w / n);
      g.fillRect(x0, barTop, Math.max(1, x1 - x0 - (w / n > 4 ? 1 : 0)), barH);
    }
  } else {
    for (let x = 0; x < w; x++) {
      const a = Math.floor(x * n / w), b = Math.max(a + 1, Math.floor((x + 1) * n / w));
      let lo = "2";
      for (let i = a; i < b; i++) if (line[i] < lo) lo = line[i];
      g.fillStyle = colors[lo];
      g.fillRect(x, barTop, 1, barH);
    }
  }
  if (st.render) {
    g.fillStyle = "#237bd3";
    g.fillRect(0, h - Math.round(5 * dpr), w * st.render.frame / n, Math.round(5 * dpr));
  }
  const px = (v.view + 0.5) * w / n;
  g.fillStyle = "#fff";
  g.fillRect(Math.round(px - dpr), 0, Math.max(2, Math.round(2 * dpr)), h);
}

function clock(sec) {
  sec = Math.max(0, sec);
  const m = Math.floor(sec / 60), s = Math.floor(sec % 60);
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}
function duration(sec) {
  if (sec == null || !isFinite(sec)) return "working it out…";
  sec = Math.round(sec);
  const h = Math.floor(sec / 3600), m = Math.floor(sec % 3600 / 60), s = sec % 60;
  if (h) return `${h} h ${m} min`;
  if (m) return `${m} min ${s} s`;
  return `${s} s`;
}

// ---------------------------------------------------------------- render

function render() {
  const err = localError || (st && st.error);
  $("error").hidden = !err;
  if (err) $("error-text").textContent = err;
  if (!st || !st.settings) return;

  const ready = !!st.ready && connected;
  const rendering = !!st.render;
  const isVideo = st.kind === "video";
  const s = st.settings;
  setFormatOptions(isVideo ? "video" : "image");
  fillSettings(s);

  view.classList.toggle("empty", !st.ready);
  $("drop-hint").hidden = !!st.ready || !!st.loading;
  $("picture").hidden = !st.ready;

  if (st.loading) {
    $("busy").hidden = false;
    $("busy-text").textContent = st.loading.what + "…";
    $("busy-bar").style.width = `${Math.round((st.loading.progress || 0) * 100)}%`;
  } else if (!uploading) {
    $("busy").hidden = true;
  }

  $("play").disabled = !ready || rendering;
  $("play").textContent = st.playing ? "Pause" : "Play";
  const pb = $("pause-build");
  pb.hidden = !(ready && isVideo && s.build_on);
  pb.disabled = !(st.video && (st.video.frontier < st.video.frames || rendering));
  pb.textContent = st.build_paused ? "Resume building" : "Pause building";
  $("hold-original").disabled = !ready;
  $("open").disabled = rendering;

  let info = "";
  if (st.ready) {
    const parts = [`shapes ${st.shapes}/${s.count}`];
    if (st.score != null) parts.push(`score ${st.score.toFixed(4)}` + (s.target_score > 0 ? ` (stop at ${s.target_score})` : ""));
    if (isVideo) parts.push(`frame ${st.video.view + 1}/${st.video.frames}`);
    if (isVideo && s.build_on && st.video.frontier < st.video.frames) parts.push(st.build_paused ? `paused at frame ${st.video.frontier + 1}` : `building frame ${st.video.frontier + 1}`);
    info = parts.join("  ·  ");
  }
  $("info").textContent = info;

  $("timeline-row").hidden = !isVideo;
  if (isVideo) {
    drawTimeline();
    const fps = st.video.fps;
    $("time").textContent = `${clock(st.video.view / fps)} / ${clock(st.video.frames / fps)}`;
  }
  $("render-row").hidden = !rendering;
  if (rendering) {
    const r = st.render;
    $("render-row").textContent = (st.build_paused ? "Paused. " : "") + `Rendering frame ${Math.min(r.frame + 1, r.frames)} of ${r.frames} — time left: ${duration(r.eta)}`;
  }

  // Settings are locked during a render.
  for (const el of $("panel").querySelectorAll("input, select")) {
    el.disabled = rendering;
  }
  $("video-mode").hidden = !isVideo;
  $("realtime-row").hidden = !(isVideo && s.build_on);
  $("fps-row").hidden = !(isVideo && st.video.sequence);
  $("custom-size").hidden = $("out_preset").value !== "custom";
  $("frame-opts").hidden = !$("export_fmt").value.endsWith("-seq");
  $("export-box").hidden = !st.ready;
  $("name-example").textContent = st.name_example ? `Example: ${st.name_example}` : "";
  $("out-dims").textContent = st.ready ? `Output: ${st.out[0]} × ${st.out[1]} px` : "";
  $("gpu").textContent = st.gpu ? `GPU: ${st.gpu}` : "";

  const restart = $("restart");
  restart.disabled = !ready || rendering;
  restart.textContent = st.needs_restart ? "Restart to apply" : "Restart";
  restart.classList.toggle("attention", !!st.needs_restart);

  const rb = $("render");
  if (rendering) {
    rb.textContent = "Cancel";
    rb.className = "danger";
    rb.disabled = false;
  } else if (rb.textContent !== "Exporting…" && rb.textContent !== "Making video…") {
    rb.className = "primary";
    rb.textContent = isVideo ? "Render" : "Export";
    rb.disabled = !ready;
  }

  const link = $("render-link");
  if (st.render_file && st.render_folder) {
    link.innerHTML = "";
    link.append(`Done. Saved in ${st.renders_dir}/${st.render_file}/`, document.createElement("br"));
    const a = document.createElement("a");
    a.href = `/zip/${encodeURIComponent(st.render_file)}`;
    a.textContent = "Download as a zip file";
    a.download = `${st.render_file}.zip`;
    link.append(a);
    myRender = false;
  } else if (st.render_file) {
    const url = `/download/${encodeURIComponent(st.render_file)}`;
    link.innerHTML = "";
    const a = document.createElement("a");
    a.href = url;
    a.textContent = `Download ${st.render_file}`;
    a.download = st.render_file;
    link.append("Done. ", a, document.createElement("br"));
    const where = document.createElement("span");
    where.className = "hint";
    where.textContent = `Also saved in ${st.renders_dir}`;
    link.append(where);
    if (myRender && downloaded !== st.render_file) {
      downloaded = st.render_file;
      myRender = false;
      a.click();
    }
  } else {
    link.innerHTML = "";
  }
}

window.addEventListener("resize", () => { if (st && st.video) drawTimeline(); });
connect();

// ---- Help boxes ------------------------------------------------------------
// Rest the mouse on a setting for a moment and a box with notes about it
// opens beside the panel. Each entry: [title, what it does, tip, example].
const TIPS = {
  count: ["Shapes",
    "How many shapes make up the picture. For a video, this is the number of shapes in each frame.",
    "Around 100 gives a bold, abstract look. 500 to 2000 starts to look like the photo. You can raise it while a picture is running and it carries on from where it is.",
    "250 triangles is a good first look at a portrait."],
  target_score: ["Stop at score",
    "Stops a picture early once its score is this low or lower. A lower score means closer to the original. The Shapes number is then the most shapes a picture may use. Leave it empty to always use the full number of shapes.",
    "For a video, each frame stops at the same quality, so easy frames use fewer shapes and busy frames use more. Each shape improves the score less than the one before, so a slightly lower target can take many more shapes. Watch the score while a picture runs to find a good value. With \"build on\", a frame that barely changes may need almost no new shapes.",
    "Try a value a little below the score you see after 250 shapes, such as 0.04."],
  min_count: ["Fewest shapes",
    "Each picture gets at least this many shapes before Stop at score may stop it. It does nothing when Stop at score is empty.",
    "Dark frames reach a low score with very few shapes, because the score measures differences in brightness and a dark frame has small differences even where it has detail. A minimum keeps those frames from looking empty.",
    "Set it to the shape count where a dark frame starts to look right, such as 50."],
  mode: ["Mode",
    "The kind of shape that is used. Combo picks the best of all kinds for each new shape.",
    "Triangles and polygons give a cut-glass look. Ellipses and circles look soft, like paint dabs. Curve draws thin strokes and is the slowest mode.",
    "Rotated rectangle with a low alpha looks like brush strokes."],
  "alpha-range": ["Alpha",
    "How see-through each shape is. 255 is solid, 1 is almost invisible.",
    "0 lets the program choose a different value for each shape. Low values give soft, layered pictures but need more shapes. High values give flat, poster-like pictures.",
    "128 (half see-through) is the default and suits most pictures."],
  background: ["Background",
    "The color of the empty canvas before the first shape is drawn.",
    "Leave it empty to use the average color of the picture, which usually gives the best result. Type a hex code or use the color picker for a set color. Restart to see the change.",
    "#000000 for black, #ffffff for white."],
  "work-range": ["Working size",
    "The picture is shrunk to this size (long side, in pixels) while the program searches for shapes.",
    "Bigger values find smaller details but each shape takes longer. It does not change how sharp the result is: the final picture is drawn again at the output size. Restart to apply.",
    "256 is fast. 512 suits large or detailed pictures."],
  out_preset: ["Output size",
    "The size of the exported picture or video.",
    "The shapes are drawn again at this size, so a big output stays sharp. It does not change which shapes are found. Custom lets you type an exact width and height.",
    "Pick 4K for a print or a wallpaper."],
  repeat: ["Extra shapes per step",
    "After each new shape, the program tries to add up to this many more by changing that last shape a little.",
    "This is faster than a full search for every shape, but the extra shapes are a bit worse. It stops early when an extra shape does not help. 0 turns it off.",
    "Try 2 or 3 when you want many shapes quickly."],
  "video-mode": ["Video frames",
    "How each frame of a video or image sequence is made.",
    "Fresh: every frame starts from an empty canvas, so each frame is its own picture and the shapes jump around. Build on: new shapes are drawn on top of the previous frame, so the picture changes smoothly and slowly turns into the next frame.",
    "Build on looks good for slow camera moves."],
  realtime: ["Build in real time",
    "Each frame gets shapes for exactly as long as it lasts in the video, then the next frame starts. The build runs at playback speed, so a scene change wipes in while you watch and motion leaves trails. The render saves the same shapes you saw.",
    "Shapes is then the most shapes a frame may get; set it high so the clock decides. The other search settings choose the look: fast settings give many rough shapes per frame, slow settings give few careful ones. The result depends on how fast the computer is at that moment. Frames that are not built yet when you press Render are built at the same speed.",
    "Press Play, watch the build, then Render when the last frame is done."],
  seq_fps: ["Frame rate",
    "How many pictures of the image sequence are shown each second.",
    "Pictures have no frame rate of their own, so you choose it here. It changes playback and the exported video length.",
    "24 for a film look, 12 for a stop-motion look."],
  chains: ["Chains",
    "How many separate searches run side by side for each new shape. The best result wins.",
    "More chains find better shapes but each shape takes longer.",
    "Default 16."],
  random: ["Random tries per chain",
    "How many random shapes each chain looks at before it starts to improve the best one.",
    "More tries give each chain a better starting point. Lower it for speed.",
    "Default 1000."],
  age: ["Age",
    "How many changes in a row a chain may try with no improvement before it stops.",
    "Higher values let a chain polish its shape for longer. Lower values are faster but the shapes are rougher.",
    "Default 100."],
  k: ["Mutations per round",
    "How many small changes each chain tries at the same time in each round.",
    "The graphics card checks them all at once, so a higher value can give better shapes at almost no extra time.",
    "Default 32."],
  metric: ["Score",
    "How the program measures the gap between the drawing and the original.",
    "Squared gaps: big mistakes count much more than small ones, so the program chases bright spots and fine detail. Plain gaps: every mistake counts the same, so flat areas stay cleaner and the shapes look bolder, more like a poster. The two give different score numbers, so set Stop at score again after you switch. Restart to apply.",
    "Try Plain gaps with triangles or rectangles."],
  seed: ["Seed",
    "The starting number for the random choices.",
    "Empty gives a new result each time. Type any number to get the same result again with the same settings. Restart to apply.",
    "Type 1, note the result, then try 2 to compare."],
  export_fmt: ["File type",
    "The kind of file that Render makes.",
    "PNG keeps every pixel exact. JPG is smaller. SVG keeps the shapes as shapes, so you can scale it to any size or edit it in Inkscape or Illustrator. For a video, the sequence types save one file per frame in a folder.",
    "Use SVG to print very large."],
  name_pattern: ["File name",
    "The name of the saved file. The tags in curly brackets are replaced with real values.",
    "{name} is the name of the source file, {shapes} the shape count, {mode} the shape kind, {date} and {time} when it was saved, {frame} the frame number. A file is never overwritten: -2, -3 is added instead.",
    "{name}-{mode}-{shapes} gives owl-triangle-250.png."],
  frame_digits: ["Frame digits",
    "How many digits the frame number has. Short numbers get zeros in front.",
    "Most editing programs need the zeros to put the frames in the right order. 0 means no zeros are added.",
    "4 gives 0001, 0002 … 0120."],
  frame_start: ["First frame",
    "The number of the first frame in the file names.",
    "Some programs expect frames to start at 0, others at 1.",
    "1 gives 0001 as the first file."],
};

(function setupTips() {
  const box = $("tip");
  // `dismissed`: the setting the user clicked or typed in; its box stays
  // closed until the mouse leaves it.
  let timer = null, current = null, dismissed = null;
  for (const [id, [title, what, tip, example]] of Object.entries(TIPS)) {
    const el = $(id);
    if (!el) continue;
    const host = el.tagName === "DETAILS" ? el.querySelector("summary") : (el.closest("label") || el);
    host.dataset.tip = id;
    host._tip = `<b>${title}</b><p>${what}</p><p><span class="tip-label">Tip:</span> ${tip}</p>` +
                `<p><span class="tip-label">Example:</span> ${example}</p>`;
  }
  function hide() {
    clearTimeout(timer);
    current = null;
    box.classList.remove("show");
    box.hidden = true;
  }
  function show(host) {
    box.innerHTML = host._tip;
    box.hidden = false;
    const r = host.getBoundingClientRect();
    const bw = box.offsetWidth, bh = box.offsetHeight;
    // Beside the panel on the left when there is room, else below the setting.
    let x = r.left - bw - 12, y = r.top;
    if (x < 8) { x = Math.min(r.left, innerWidth - bw - 8); y = r.bottom + 6; }
    y = Math.max(8, Math.min(y, innerHeight - bh - 8));
    box.style.left = x + "px";
    box.style.top = y + "px";
    box.classList.add("show");
  }
  document.addEventListener("mouseover", (e) => {
    const host = e.target.closest("[data-tip]");
    if (host === current) return;
    hide();
    if (host !== dismissed) dismissed = null;
    if (!host || host === dismissed) return;
    current = host;
    timer = setTimeout(() => show(host), 700);
  });
  const dismiss = () => { dismissed = current; hide(); current = dismissed; };
  document.addEventListener("mousedown", dismiss);
  document.addEventListener("keydown", dismiss);
  $("panel-body").addEventListener("scroll", hide);
  window.addEventListener("blur", hide);
})();
