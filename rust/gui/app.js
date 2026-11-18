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
async function ask(key, text, ok = "Go on") {
  if (localStorage.getItem(SKIP + key)) return true;
  return popup(text, { key, ok });
}
function tell(text) { return popup(text, { ok: "OK", cancel: null }); }
function showAskReset() { $("ask-reset").hidden = skipped().length === 0; }
$("ask-reset").onclick = () => { for (const k of skipped()) localStorage.removeItem(k); showAskReset(); };
showAskReset();
let ws = null;
let st = null;               // newest status from the program
let picUrl = null;
let picBlob = null;          // the picture on screen, kept for Snapshot
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
  ws.onopen = () => { connected = true; localError = null; if (maskData) sendMask(); render(); };
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
      const blob = ev.data;
      $("picture").onload = () => { if (picUrl && picUrl !== url) URL.revokeObjectURL(picUrl); picUrl = url; picBlob = blob; };
      $("picture").src = url;
    }
  };
}

function send(obj) {
  if (ws && ws.readyState === 1) ws.send(JSON.stringify(obj));
}

// -------------------------------------------------------------- settings

const NUM = ["count", "min_count", "mode", "alpha", "work_size", "out_w", "out_h", "repeat", "chains", "random", "age", "k", "frame_digits", "frame_start", "c_grid"];
const FLOAT = ["stop_floor", "seq_fps", "c_angle_deg", "c_angle_step", "c_x", "c_y", "c_min_size", "c_max_size", "c_min_ratio", "c_max_ratio"];
const TEXT = ["background", "metric", "c_angle", "c_area", "out_preset", "export_fmt", "name_pattern"];
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
  const sm = parseFloat($("stop_match").value);
  s.stop_match = Number.isFinite(sm) && sm > 0 && sm < 100 ? sm : 0;
  s.build_on = document.querySelector('input[name=frames][value=build]').checked;
  s.realtime = $("realtime").checked;
  s.loop_video = $("loop_video").checked;
  s.c_shrink = $("c_shrink").checked;
  // Unticked "Manual seed" sends an empty seed: a new random one each Restart.
  s.seed = $("seed_lock").checked ? $("seed").value.trim() : "";
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
  const tsEl = $("stop_match");
  if (tsEl !== document.activeElement && !typing) {
    const v = s.stop_match > 0 ? String(s.stop_match) : "";
    if (tsEl.value !== v) tsEl.value = v;
  }
  if (!typing) {
    $("alpha-range").value = s.alpha;
    $("work-range").value = s.work_size;
    document.querySelector(`input[name=frames][value=${s.build_on ? "build" : "fresh"}]`).checked = true;
    $("realtime").checked = !!s.realtime;
    $("loop_video").checked = !!s.loop_video;
    $("c_shrink").checked = !!s.c_shrink;
    $("c_angle_deg-range").value = ((s.c_angle_deg % 180) + 180) % 180;
    if (/^#?[0-9a-fA-F]{6}$/.test(s.background)) $("background-pick").value = "#" + s.background.replace("#", "");
  }
  fillSeed(s, typing);
  showRuleRows();
}

// Seed: unticked shows the seed in use, greyed out. Ticked keeps the number
// and makes it editable. Ticking keeps the number that is showing.
function fillSeed(s, typing) {
  const lock = $("seed_lock"), el = $("seed");
  if (!typing) lock.checked = s.seed !== "";
  el.disabled = !lock.checked;
  if (el === document.activeElement || typing) return;
  const v = lock.checked ? s.seed : ((st && st.seed) || "");
  if (el.value !== v) el.value = v;
  el.placeholder = lock.checked ? "type a number" : "random";
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
  if ((await applySettings(loaded)) === "none") tell(`${f.name} has no settings in it.`);
});

// Puts loaded settings into the panel and sends them. A setting that is
// missing, or of the wrong type, keeps its current value. Gives "none" when
// nothing matched, "cancel" when the user said no, "ok" when applied.
async function applySettings(loaded) {
  const cur = readSettings();
  const known = Object.keys(cur).filter((k) => loaded && k in loaded && typeof loaded[k] === typeof cur[k]);
  if (!known.length) return "none";
  const next = { ...cur };
  for (const k of known) next[k] = loaded[k];
  // A file type that this kind of source does not offer keeps the current one.
  if (![...$("export_fmt").options].some((o) => o.value === next.export_fmt)) next.export_fmt = cur.export_fmt;
  if (next.build_on !== cur.build_on && st && st.has_work &&
      !(await ask("switch-frames", "These settings switch how video frames are built. That clears the shapes of every frame. Go on?"))) return "cancel";
  lastEdit = 0;
  fillSettings(next);
  sendSettings();
  return "ok";
}
$("stop_match").addEventListener("change", sendSettings);
// An empty title stops the browser's own "select a valid value" popup.
for (const el of document.querySelectorAll("input")) if (!el.title) el.setAttribute("title", "");
$("realtime").addEventListener("change", sendSettings);
$("loop_video").addEventListener("change", sendSettings);
$("c_shrink").addEventListener("change", sendSettings);
$("c_angle").addEventListener("input", showRuleRows);
$("c_area").addEventListener("input", showRuleRows);
$("c_angle_deg-range").addEventListener("input", () => { $("c_angle_deg").value = $("c_angle_deg-range").value; sendSettings(); });
$("seed").addEventListener("change", sendSettings);
$("seed_lock").addEventListener("change", () => { $("seed").disabled = !$("seed_lock").checked; sendSettings(); });
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
    // Status updates (many per second while a video plays) refill the panel
    // while the question is open and can flip the radio back. Use the choice
    // that was clicked.
    r.checked = true;
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

let exporting = false; // a single-image export is downloading

$("render").onclick = async () => {
  if (!st) return;
  if (st.render) { send({ t: "cancel" }); myRender = false; return; }
  if (st.kind === "video") { myRender = true; send({ t: "render" }); return; }
  const fmt = $("export_fmt").value;
  exporting = true;
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
  exporting = false;
  render();
};

// A video: save only the frame on screen as one picture.
let savingFrame = false;
$("save-frame").onclick = async () => {
  if (!st || st.kind !== "video" || savingFrame) return;
  const seq = $("export_fmt").value;
  const fmt = seq === "jpg-seq" ? "jpg" : seq === "svg-seq" ? "svg" : "png";
  savingFrame = true;
  render();
  try {
    const r = await fetch(`/export?fmt=${fmt}`);
    if (!r.ok) throw new Error(await r.text());
    const blob = await r.blob();
    const m = (r.headers.get("content-disposition") || "").match(/filename="([^"]+)"/);
    saveBlob(blob, m ? m[1] : `frame.${fmt}`);
  } catch (e) {
    localError = `Save frame failed: ${e.message}`;
  }
  savingFrame = false;
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

// ----------------------------------------------------------------- slots
// Snapshot keeps the picture on screen, the settings and the painted area
// in a slot on this page (lost on reload). Clicking a slot, or pressing its
// number key, shows its picture and puts its settings back. The same key,
// a click on the same slot, or Esc goes back to the live picture.
const SLOTS = 8;
const slots = new Array(SLOTS).fill(null);
let viewing = -1;            // slot on screen, -1 = the live picture

function drawSlots() {
  const bar = $("slots");
  bar.innerHTML = "";
  bar.hidden = !slots.some(Boolean);
  slots.forEach((s, i) => {
    if (!s) return;
    const b = document.createElement("div");
    b.className = "slot" + (i === viewing ? " on" : "");
    b.title = `Slot ${i + 1}: ${s.label}. Click or press ${i + 1} to view it and use its settings.`;
    b.innerHTML = `<span class="slot-num">${i + 1}</span><img alt=""><button class="slot-x" title="Empty this slot">&times;</button>`;
    b.querySelector("img").src = s.url;
    b.onclick = () => openSlot(i);
    b.querySelector(".slot-x").onclick = (e) => { e.stopPropagation(); emptySlot(i); };
    bar.append(b);
  });
  const v = $("slot-view");
  if (viewing >= 0) { v.src = slots[viewing].url; v.hidden = false; } else v.hidden = true;
}

function snapshot() {
  if (!st || !st.ready || !picBlob) return;
  const i = slots.indexOf(null);
  if (i < 0) { tell(`All ${SLOTS} slots are full. Empty one with its × first.`); return; }
  let mask = null;
  if (maskData) {
    mask = document.createElement("canvas");
    mask.width = maskData.width; mask.height = maskData.height;
    mask.getContext("2d").drawImage(maskData, 0, 0);
  }
  slots[i] = { url: URL.createObjectURL(picBlob), settings: readSettings(), mask,
               invert: $("mask-invert").checked, label: st.video ? `frame ${st.video.view + 1}` : "picture" };
  drawSlots();
}

function emptySlot(i) {
  if (!slots[i]) return;
  URL.revokeObjectURL(slots[i].url);
  slots[i] = null;
  if (viewing === i) viewing = -1;
  drawSlots();
}

function closeSlot() { if (viewing >= 0) { viewing = -1; drawSlots(); } }

async function openSlot(i) {
  const s = slots[i];
  if (!s) return;
  if (viewing === i) { closeSlot(); return; }
  if (st && st.render) { tell("A render is running. Cancel it first."); return; }
  if (!(await ask("slot-load", `Viewing slot ${i + 1} replaces your current settings and painted area with the ones saved in the slot. Go on?`, "OK"))) return;
  if ((await applySettings(s.settings)) === "cancel") return;
  maskData = null;
  if (s.mask) {
    maskData = document.createElement("canvas");
    maskData.width = s.mask.width; maskData.height = s.mask.height;
    maskData.getContext("2d").drawImage(s.mask, 0, 0);
  }
  $("mask-invert").checked = s.invert;
  sendMask();
  drawOverlay();
  viewing = i;
  drawSlots();
}

$("snapshot").onclick = snapshot;

// Number keys 1 to 8 open a slot, Esc goes back to the live picture.
// Not while the cursor is in a setting or a popup is open.
document.addEventListener("keydown", (e) => {
  if (e.ctrlKey || e.altKey || e.metaKey || $("ask").open) return;
  if (e.target.closest && e.target.closest("input:not([type=checkbox]), select, textarea")) return;
  if (e.key === "Escape" && viewing >= 0) { e.preventDefault(); closeSlot(); return; }
  const n = parseInt(e.key, 10);
  if (n >= 1 && n <= SLOTS && String(n) === e.key && slots[n - 1]) { e.preventDefault(); openSlot(n - 1); }
});

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

// Arrow keys: left / right move one frame, up jumps to the last frame,
// down to the first. Not while the cursor is in a setting, so the keys
// still change that setting.
let keyFrame = 0, keyTime = 0;
document.addEventListener("keydown", (e) => {
  if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) return;
  if (!st || !st.video || st.render || e.ctrlKey || e.altKey || e.metaKey || $("ask").open) return;
  if (e.target.closest && e.target.closest("input:not([type=checkbox]), select, textarea")) return;
  e.preventDefault();
  const v = st.video;
  const last = st.settings.build_on ? Math.min(v.frames - 1, v.frontier) : v.frames - 1;
  // Held key: count from the last frame asked for, not the last one shown.
  const now = Date.now();
  const from = now - keyTime < 500 ? keyFrame : v.view;
  const f = e.key === "ArrowUp" ? last : e.key === "ArrowDown" ? 0 : from + (e.key === "ArrowRight" ? 1 : -1);
  keyFrame = Math.max(0, Math.min(last, f));
  keyTime = now;
  send({ t: "seek", frame: keyFrame });
});

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

// Build time on the status line: tenths of a second, to compare settings.
function buildTime(sec) {
  if (sec < 60) return `${sec.toFixed(1)} s`;
  const m = Math.floor(sec / 60);
  return `${m} min ${(sec - m * 60).toFixed(1).padStart(4, "0")} s`;
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
  placeOverlay();

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
  $("snapshot").disabled = !ready;
  const sf = $("save-frame");
  sf.hidden = !(st.ready && isVideo);
  sf.disabled = !ready || rendering || savingFrame;
  sf.textContent = savingFrame ? "Saving…" : "Save frame";
  $("open").disabled = rendering;

  let info = "";
  if (st.ready) {
    const parts = [`shapes ${st.shapes}/${s.count}`];
    if (st.score != null) parts.push(`score ${st.score.toFixed(4)}`);
    if (st.matched != null) parts.push(`match ${Math.floor(st.matched)}%` + (st.stop_score == null ? "" : st.stop_floor ? ` (flat picture: stops at score ${st.stop_score.toFixed(3)})` : ` (stop at ${s.stop_match}%)`));
    if (st.time != null && st.shapes > 0) parts.push(`time ${buildTime(st.time)}`);
    if (isVideo) parts.push(`frame ${st.video.view + 1}/${st.video.frames}`);
    if (isVideo && s.build_on && st.video.frontier < st.video.frames) parts.push(st.build_paused ? `paused at frame ${st.video.frontier + 1}` : `building frame ${st.video.frontier + 1}`);
    if (isVideo && st.video.loop_prep) parts.push(st.video.pass2_next < st.video.frames
      ? `building the loop: frame ${st.video.pass2_next + 1}/${st.video.frames}` : "preparing the loop");
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
    const frameOf = `frame ${Math.min(r.frame + 1, r.frames)} of ${r.frames}`;
    $("render-row").textContent = (st.build_paused ? "Paused. " : "") +
      (r.prep ? `Preparing the loop: ${frameOf}` : `Rendering ${frameOf} — time left: ${duration(r.eta)}`);
  }

  // Settings are locked during a render.
  for (const el of $("panel").querySelectorAll("input, select")) {
    el.disabled = rendering;
  }
  $("video-mode").hidden = !isVideo;
  $("realtime-row").hidden = !(isVideo && s.build_on);
  $("loop-row").hidden = !(isVideo && s.build_on);
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
  } else if (!exporting) {
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

window.addEventListener("resize", () => { if (st && st.video) drawTimeline(); placeOverlay(); });
connect();

// ---- Help boxes ------------------------------------------------------------
// Rest the mouse on a setting for a moment and a box about it opens beside
// the panel. Each entry: [title, what it does] or, for a number, [title, what
// it does, what higher means, what lower means].
const TIPS = {
  count: ["Shapes","How many shapes make up the picture. For a video, this is the number per frame. With \"build on\", it is the number of new shapes that each frame adds.","Closer to the photo, but slower.","Bolder and more abstract."],
  stop_match: ["Stop at match","Each picture stops as soon as this much of it is matched, or at the Shapes number. 0% is a flat canvas in the picture's average color, 100% is the original. The status line shows the match of the picture on screen. Off: every frame gets the same number of shapes. On: every frame ends up equally finished, so simple frames use few shapes and take little time, and detailed frames use many. A dark frame counts the same as a bright one. A nearly flat frame stops early, so shapes do not chase noise. Try 70. Empty: off.","Takes more time, mostly on detailed frames. The last few percent cost the most shapes.","Stops sooner. Detailed frames save the most time."],
  stop_floor: ["Lowest score","Stop at match never asks a picture for a score below this number. It is in the score units of the status line. It is for dark or nearly flat frames: their last bit of contrast is mostly grain, and shapes cannot match grain. To set it, watch the score on such a frame and type a number near the score where it looks finished. The number means something else for each Score choice, so check it after you switch. It does nothing when Stop at match is empty. 0: off.","Dark or nearly flat frames stop sooner and can look emptier.","Dark or nearly flat frames get more shapes. Too low, and shapes chase grain."],
  min_count: ["Fewest shapes","Least shapes a picture gets before Stop at match may end it. It does nothing when Stop at match is empty. With \"build on\", it keeps a still scene from freezing.","Still or nearly flat frames keep changing.","Pictures stop earlier."],
  mode: ["Mode","The kind of shape used. Combo picks the best kind for each new shape."],
  "alpha-range": ["Alpha","How see-through each shape is. 255 is solid. 0: the program picks a value for each shape."],
  background: ["Background","Color of the canvas under the first shape. Empty: the average color of the picture. Restart to apply."],
  "work-range": ["Working size","Size the picture is shrunk to while the program searches (long side, in pixels). The final picture is drawn again at the output size. Restart to apply.","Finds finer detail, but slower.","Faster, but rougher."],
  out_preset: ["Output size","Size of the exported picture or video. Shapes are drawn again at this size, so a big output stays sharp. Custom: the picture fits inside the width and height you type and keeps its shape."],
  repeat: ["Extra shapes per step","After each new shape, tries to add up to this many more by nudging it. It stops early when an extra shape does not help. 0: off.","Many shapes faster, but the extra ones are a bit worse.","Slower, but better shapes."],
  "video-mode": ["Video frames","Fresh: each frame starts empty, and the shapes jump around. Build on: new shapes go over the previous frame, so the picture changes smoothly."],
  realtime: ["Build in real time","Each frame gets shapes only for as long as it lasts in the video, so Shapes becomes the most a frame may get. A frame that reaches Shapes or Stop at match early waits for its time. A faster computer builds more shapes per frame. The result is different each time, even with the same seed."],
  loop_video: ["Loop video","Makes the end of the video join up with the start. Every frame is built a second time, and the first frame starts from the last picture. This takes as long as the first build."],
  "rules-box": ["Shape rules","Rules that every new shape must follow. They only affect shapes found from now on. Press Restart to see them on the whole picture."],
  c_angle: ["Direction","Which way shapes lean. One angle: all the same. Steps: only a few angles. Point at a spot: shapes aim at it, like rays. Rings around a spot: shapes run around it. Only affects rotated rectangles, rotated ellipses and curves."],
  c_angle_step: ["Step","Degrees between the allowed angles, counted from Angle.","Fewer allowed directions, a tidy woven look.","More allowed directions, closer to free."],
  c_angle_deg: ["Angle","Direction of the long side of every shape. 0 is flat, 90 is upright. In Steps mode, it is the angle that the steps start from."],
  "mask-invert": ["Invert","Swaps the painted and unpainted areas. It does nothing when nothing is painted."],
  c_min_size: ["Smallest size","The long side of each shape is at least this (% of the picture's long side). With \"Small shapes here, big elsewhere\" and a painted area, it is the largest size in the painted area instead. 0: off.","Bold poster look, no small details.","Allows fine detail."],
  c_max_size: ["Largest size","The long side of each shape is at most this (% of the picture's long side). With \"Small shapes here, big elsewhere\" and a painted area, it is the size of the shapes outside the painted area instead. 0: off.","Big, blocky shapes.","Confetti look, needs many more shapes to fill the picture."],
  c_shrink: ["Shrink as shapes are added","The largest allowed size falls from Largest size to Smallest size by the last shape: big shapes first, fine detail last. If Smallest size is off, it ends at 2%. If Largest size is off, it starts at the full picture. For a video, it starts again on every frame. When Stop at match ends a picture early, the smallest size is not reached."],
  c_min_ratio: ["At least this long","Each shape is at least this many times longer than wide. Only affects rectangles and ellipses, plain and rotated. 1 or less: off.","Thinner strokes.","Chunkier shapes."],
  c_max_ratio: ["At most this long","No shape is more than this many times longer than wide. 1: only squares or round ellipses. Only affects rectangles and ellipses, plain and rotated. 0: off.","More stretched shapes allowed.","Closer to square."],
  c_grid: ["Grid cells","Shapes snap to a grid with this many cells along the picture's long side. Rectangles fill whole cells. Circles, ellipses and rotated rectangles sit in the middle of a cell. The corners of triangles and polygons, and the points of curves, go to grid crossings. 0: off.","Finer mosaic.","Coarser, blockier."],
  c_area: ["Painted area means","What the area you paint does. Only place shapes here: the middle and the edges of each new shape must be inside the painted area. Small shapes here, big elsewhere: in the painted area, shapes are at most Smallest size. In the rest, they are from half of Largest size up to Largest size. The size changes softly at the edge of the area. A size that is off counts as 3% (small) or 20% (big)."],
  "mask-paint": ["Paint area","Paint the area that \"Painted area means\" uses. It affects new shapes at once. Press Restart to see it on the whole picture. Nothing painted: no limit. The area is not saved with the settings. It is cleared when the page reloads or when a picture of another shape is loaded."],
  seq_fps: ["Frame rate","Pictures shown each second.","Faster playback, shorter video.","Slower playback, longer video."],
  chains: ["Chains","Searches that run side by side for each new shape. The best one wins.","Better shapes, but slower.","Faster, but rougher shapes."],
  random: ["Random tries per chain","Random shapes each chain looks at before it starts to improve the best one.","Better starting point, but slower.","Faster."],
  age: ["Age","Failed changes in a row before a chain gives up. It counts single changes, not rounds: with 64 mutations per round, an age of 100 ends a chain after 2 rounds without gain.","More polished shapes, but slower.","Faster, but rougher shapes."],
  k: ["Mutations per round","Small changes each chain tries at once in each round. The best one is kept if it helps. Age counts these changes, so with a higher value a chain gives up after fewer rounds.","Picks from more changes each round. The graphics card checks them together, so it costs little extra time.","Fewer changes per round, so a chain takes smaller steps."],
  metric: ["Score","How the gap to the original is measured. Squared gaps: big mistakes count most, so it chases bright spots and fine detail. Plain gaps: all mistakes count the same, so it looks bolder and cleaner. The two give different score and match numbers, so check Stop at match and Lowest score again after you switch. Restart to apply."],
  seed: ["Seed","Starting number for the random choices. The same seed and settings give the same result, except with Build in real time. With Manual seed off, each Restart picks a new seed. Restart to apply."],
  export_fmt: ["File type","What Render makes. PNG keeps every pixel exact. JPG is smaller. SVG keeps the shapes as shapes, so it stays sharp at any size. For a single picture, MP4, WebM and GIF make a video of the shapes being added one by one. For a video, the sequence types save one file per frame."],
  name_pattern: ["File name","Name of the saved file. Tags in curly brackets are replaced: {name} the source file, {shapes} the shape count, {mode} the shape kind, {date} and {time} when it was saved, {frame} the frame number (left out for a single picture). Nothing is overwritten; -2, -3 is added instead."],
  frame_digits: ["Frame digits","Digits in the frame number. 4 gives 0001, 0002. 0: no zeros."],
  frame_start: ["First frame","Number of the first frame in the file names. Some programs expect 0, others 1."],
};

(function setupTips() {
  const box = $("tip");
  // `dismissed`: the setting the user clicked or typed in; its box stays
  // closed until the mouse leaves it.
  let timer = null, current = null, dismissed = null;
  for (const [id, [title, what, higher, lower]] of Object.entries(TIPS)) {
    const el = $(id);
    if (!el) continue;
    const host = el.tagName === "DETAILS" ? el.querySelector("summary") : (el.closest("label") || el);
    host.dataset.tip = id;
    host._tip = `<b>${title}</b><p>${what}</p>` +
      (higher ? `<p><span class="tip-label">Higher:</span> ${higher}</p><p><span class="tip-label">Lower:</span> ${lower}</p>` : "");
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

// ---------------------------------------------------------- shape rules

// Show only the Direction rows that the chosen direction uses.
function showRuleRows() {
  const a = $("c_angle").value;
  $("c-angle-row").hidden = !(a === "fixed" || a === "steps");
  $("c-step-row").hidden = a !== "steps";
  $("c-spot-row").hidden = !(a === "toward" || a === "around");
  $("mask-hint").textContent = $("c_area").value === "detail"
    ? "Paint where the small shapes go. Nothing painted = normal sizes everywhere."
    : "Paint where shapes may go. Nothing painted = no limit.";
  drawOverlay();
}

// The layer over the picture. It shows the painted area while painting and
// the spot that the shapes point at.
const overlay = $("overlay"), maskCv = $("mask-canvas");
const MASK_LONG = 128;
let maskData = null;   // small canvas; painted (opaque) = shapes allowed
let maskOn = false, maskDrawing = false, maskLast = null, spotPicking = false;

function placeOverlay() {
  const pic = $("picture");
  const show = !pic.hidden && pic.naturalWidth > 0;
  overlay.style.display = show ? "block" : "none";
  if (!show) return;
  const v = view.getBoundingClientRect(), r = pic.getBoundingClientRect();
  overlay.style.left = (r.left - v.left) + "px";
  overlay.style.top = (r.top - v.top) + "px";
  overlay.style.width = r.width + "px";
  overlay.style.height = r.height + "px";
  // A picture of another shape: the painted area no longer fits it.
  if (maskData && Math.abs(maskData.width / maskData.height - pic.naturalWidth / pic.naturalHeight) > 0.03) {
    maskData = null;
    sendMask();
  }
  const cw = Math.max(1, Math.round(r.width)), ch = Math.max(1, Math.round(r.height));
  if (maskCv.width !== cw || maskCv.height !== ch) { maskCv.width = cw; maskCv.height = ch; }
  drawOverlay();
}

function drawOverlay() {
  const c = maskCv.getContext("2d");
  c.clearRect(0, 0, maskCv.width, maskCv.height);
  if (maskOn) {
    c.fillStyle = "rgba(0, 0, 0, 0.65)";
    c.fillRect(0, 0, maskCv.width, maskCv.height);
    if (maskData) {
      // Inverted: the dark part is the painted part.
      c.globalCompositeOperation = $("mask-invert").checked ? "destination-in" : "destination-out";
      c.drawImage(maskData, 0, 0, maskCv.width, maskCv.height);
      c.globalCompositeOperation = "source-over";
    }
  }
  const a = $("c_angle").value;
  const spot = (a === "toward" || a === "around") && $("rules-box").open;
  const dot = $("spot-dot");
  dot.style.display = spot ? "block" : "none";
  if (spot) {
    dot.style.left = (parseFloat($("c_x").value) || 0) + "%";
    dot.style.top = (parseFloat($("c_y").value) || 0) + "%";
  }
  overlay.classList.toggle("active", maskOn || spotPicking);
  $("mask-clear").disabled = !maskData;
}

function sendMask() {
  let bits = "", any = false, w = 0, h = 0;
  if (maskData) {
    w = maskData.width; h = maskData.height;
    const d = maskData.getContext("2d").getImageData(0, 0, w, h).data;
    const parts = [];
    const inv = $("mask-invert").checked;
    for (let i = 3; i < d.length; i += 4) { const on = d[i] >= 128; any = any || on; parts.push(on !== inv ? "1" : "0"); }
    bits = parts.join("");
  }
  if (!any) { maskData = null; send({ t: "mask", w: 0, h: 0, bits: "" }); }
  else send({ t: "mask", w, h, bits });
}

function overlayPos(e) {
  const r = overlay.getBoundingClientRect();
  return [(e.clientX - r.left) / r.width, (e.clientY - r.top) / r.height];
}

function paintAt(e) {
  const pic = $("picture");
  if (!maskData) {
    const a = pic.naturalWidth / pic.naturalHeight;
    maskData = document.createElement("canvas");
    maskData.width = a >= 1 ? MASK_LONG : Math.max(1, Math.round(MASK_LONG * a));
    maskData.height = a >= 1 ? Math.max(1, Math.round(MASK_LONG / a)) : MASK_LONG;
  }
  const [x, y] = overlayPos(e);
  const px = x * maskData.width, py = y * maskData.height;
  const size = parseInt($("mask-brush").value, 10) / 100 * MASK_LONG;
  const c = maskData.getContext("2d");
  c.globalCompositeOperation = $("mask-erase").checked ? "destination-out" : "source-over";
  c.fillStyle = c.strokeStyle = "#fff";
  c.lineWidth = size;
  c.lineCap = "round";
  c.beginPath();
  c.arc(px, py, size / 2, 0, Math.PI * 2);
  c.fill();
  if (maskLast) {
    c.beginPath();
    c.moveTo(maskLast[0], maskLast[1]);
    c.lineTo(px, py);
    c.stroke();
  }
  maskLast = [px, py];
  drawOverlay();
}

overlay.addEventListener("pointerdown", (e) => {
  if (spotPicking) {
    const [x, y] = overlayPos(e);
    $("c_x").value = Math.round(x * 1000) / 10;
    $("c_y").value = Math.round(y * 1000) / 10;
    setSpotPicking(false);
    lastEdit = 0;
    sendSettings();
    return;
  }
  if (!maskOn) return;
  e.preventDefault();
  overlay.setPointerCapture(e.pointerId);
  maskDrawing = true;
  maskLast = null;
  paintAt(e);
});
overlay.addEventListener("pointermove", (e) => { if (maskDrawing) paintAt(e); });
for (const ev of ["pointerup", "pointercancel"]) {
  overlay.addEventListener(ev, () => { if (maskDrawing) { maskDrawing = false; sendMask(); drawOverlay(); } });
}

function setSpotPicking(on) {
  spotPicking = on;
  $("spot-pick").textContent = on ? "Click on the picture…" : "Pick the spot on the picture";
  $("spot-pick").classList.toggle("primary", on);
  if (on) setMaskOn(false);
  drawOverlay();
}
function setMaskOn(on) {
  maskOn = on;
  $("mask-paint").textContent = on ? "Done painting" : "Paint area";
  $("mask-paint").classList.toggle("primary", on);
  $("mask-tools").hidden = !on;
  if (on && spotPicking) setSpotPicking(false);
  drawOverlay();
}
$("spot-pick").onclick = () => setSpotPicking(!spotPicking);
$("mask-paint").onclick = () => setMaskOn(!maskOn);
$("mask-invert").onchange = () => { sendMask(); drawOverlay(); };
$("mask-clear").onclick = () => { maskData = null; sendMask(); drawOverlay(); };
$("rules-box").addEventListener("toggle", drawOverlay);
$("picture").addEventListener("load", placeOverlay);
for (const k of ["c_x", "c_y"]) $(k).addEventListener("input", drawOverlay);
showRuleRows();
