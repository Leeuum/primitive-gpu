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
// `alt` adds a third button; then the answer is "yes", "alt" or "no".
// `check` adds a tick box with that label; read $("ask-check").checked after.
function popup(text, { key = null, ok = "Go on", cancel = "Cancel", alt = null, check = null } = {}) {
  const d = $("ask");
  $("ask-check-row").hidden = !check;
  $("ask-check-text").textContent = check || "";
  $("ask-check").checked = false;
  $("ask-text").textContent = text;
  $("ask-yes").textContent = ok;
  $("ask-no").textContent = cancel;
  $("ask-no").hidden = !cancel;
  $("ask-alt").textContent = alt || "";
  $("ask-alt").hidden = !alt;
  $("ask-skip-row").hidden = !key;
  $("ask-skip").checked = false;
  d.returnValue = "";
  d.showModal();
  $("ask-yes").focus();
  return new Promise((done) => {
    d.addEventListener("close", () => {
      const yes = d.returnValue === "yes";
      if (yes && key && $("ask-skip").checked) { localStorage.setItem(SKIP + key, "1"); showAskReset(); }
      done(alt ? (d.returnValue || "no") : yes);
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
let picBlob = null;          // the picture on screen, kept for Snapshot
let myRender = false;        // this page pressed Render
let downloaded = null;       // render file this page already downloaded
let lastEdit = 0;            // time of the last settings edit on this page
let connected = false;
let localError = null;
let defaults = null;         // the starting value of each setting

// ------------------------------------------------------------ connection

function connect() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  ws = new WebSocket(`${proto}://${location.host}/ws`);
  ws.binaryType = "blob";
  ws.onopen = () => {
    connected = true;
    localError = null;
    if (!defaults) fetch("/defaults").then((r) => r.json()).then((d) => { defaults = d; }).catch(() => {});
    if (maskData) sendMask();
    render();
  };
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
      showPicture(ev.data);
    }
  };
}

// Shows one picture at a time. A new picture that arrives during loading
// waits, and only the newest one waits. Without this, each new picture
// stops the one before it and a fast drag shows nothing.
// The browser unpacks the JPEG in the background. The picture goes on the
// canvas at the next screen frame, so the page never stops to unpack it, and
// a picture only counts as done when it is on screen.
let picLoading = false, picWaiting = null;
function showPicture(blob) {
  if (picLoading) { picWaiting = blob; return; }
  picLoading = true;
  const done = () => {
    picLoading = false;
    if (picWaiting) { const next = picWaiting; picWaiting = null; showPicture(next); }
  };
  createImageBitmap(blob).then((bmp) => requestAnimationFrame(() => {
    const pic = $("picture");
    const newSize = !picBlob || pic.width !== bmp.width || pic.height !== bmp.height;
    if (newSize) { pic.width = bmp.width; pic.height = bmp.height; }
    pic.getContext("bitmaprenderer").transferFromImageBitmap(bmp);
    picBlob = blob;
    if (newSize) placeOverlay();
    done();
  }), done);
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

// Save writes a .primitive save file (made by the program, see save.rs):
// the recipe, where the source file is, and the shapes of a still picture.
// Load opens a save file, or loads a recipe. The recipe holds the
// settings, the keyframes and the painted areas, and no file names. A
// loaded recipe only changes the settings it names, so files from older
// versions still work. An older settings file has no "recipe" number and
// changes only the settings.
$("settings-save").onclick = async () => {
  const kind = st ? st.kind : "none";
  const name = st && st.name;
  let text, check = null;
  if (kind === "image") {
    text = `Save your work on ${name}: the settings, the keyframes, the painted areas and the shapes made so far.`;
    if (st.origin) {
      text += " The save remembers where the picture is on this computer.";
      check = "Include original picture (a bigger file, but it still works if the picture moves)";
    } else {
      text += " The picture was uploaded, so the program does not know where it is. A copy of it goes into the save.";
    }
  } else if (kind === "video" && st.video && st.video.sequence) {
    text = "Save the settings and the painted area. Image sequences are not kept in a save file.";
  } else if (kind === "video") {
    text = `Save the settings and the painted area for ${name}. The shapes of a video are not kept: it builds again from the start when you open the save.`;
    text += st.origin ? " The save remembers where the video is on this computer." : " The video was uploaded, so the save does not know where it is. You pick it again when you open the save.";
  } else {
    text = "Save the settings, the keyframes and the painted area. No file is open, so the save holds no picture.";
  }
  if (!(await popup(text, { ok: "Save", check }))) return;
  const pic = check && $("ask-check").checked ? 1 : 0;
  const dir = await folderFor("save");
  if (!dir) return;
  try {
    showBusy("Saving", 0);
    if (dir !== "download") {
      const where = await writeInto(`/save?pic=${pic}`, dir);
      if (where) savedNote(`Saved ${where}`);
      return;
    }
    const r = await fetch(`/save?pic=${pic}`);
    if (!r.ok) throw new Error(await r.text());
    const m = /filename="([^"]+)"/.exec(r.headers.get("Content-Disposition") || "");
    saveBlob(await r.blob(), m ? m[1] : "primitive-save.primitive");
  } catch (e) {
    tell(`Save failed: ${e.message}`);
  } finally {
    hideBusy();
  }
};
$("settings-load").onclick = () => $("settings-file").click();
$("settings-file").addEventListener("change", async () => {
  const f = $("settings-file").files[0];
  $("settings-file").value = "";
  if (!f) return;
  if (st && st.render) { tell("A render is running. Cancel it first."); return; }
  if (SAVE_FILE.test(f.name)) return openSave(f);
  let data;
  if (/\.(png|svg)$/i.test(f.name)) {
    data = await recipeIn(f);
    if (!data) { tell(`${f.name} has no recipe in it. Only PNG and SVG files exported by this program hold one, and websites and chat apps often remove it.`); return; }
    return loadRecipe(data, f.name);
  }
  try {
    data = JSON.parse(await f.text());
  } catch (e) {
    tell(`${f.name} is not a recipe or settings file.`);
    return;
  }
  if (!data || typeof data !== "object") { tell(`${f.name} is not a recipe or settings file.`); return; }
  // The oldest settings files hold the settings with nothing around them.
  if (typeof data.settings !== "object") data = { settings: data };
  await loadRecipe(data, f.name);
});

// The recipe inside a PNG or SVG exported by this program, or null. The
// program writes it as a PNG text chunk (iTXt) or an SVG <metadata>
// element named "primitive-recipe" (see `recipe.rs`).
const RECIPE_TAG = "primitive-recipe";
async function recipeIn(file) {
  const parse = (text) => {
    try {
      const r = JSON.parse(text);
      return r && typeof r === "object" && typeof r.recipe === "number" && typeof r.settings === "object" ? r : null;
    } catch (e) { return null; }
  };
  try {
    if (/\.svg$/i.test(file.name) || file.type === "image/svg+xml") {
      if (file.size > 64e6) return null;
      const doc = new DOMParser().parseFromString(await file.text(), "image/svg+xml");
      const el = doc.getElementById(RECIPE_TAG);
      return el ? parse(el.textContent) : null;
    }
    if (!(/\.png$/i.test(file.name) || file.type === "image/png") || file.size > 256e6) return null;
    const b = new Uint8Array(await file.arrayBuffer());
    const dv = new DataView(b.buffer);
    const sig = [137, 80, 78, 71, 13, 10, 26, 10];
    if (b.length < 8 || sig.some((v, i) => b[i] !== v)) return null;
    const latin = (a) => String.fromCharCode(...a);
    for (let at = 8; at + 12 <= b.length;) {
      const len = dv.getUint32(at);
      const type = latin(b.subarray(at + 4, at + 8));
      const data = b.subarray(at + 8, Math.min(at + 8 + len, b.length));
      if (type === "IDAT" || type === "IEND") break;   // text chunks we write come first
      if (type === "iTXt") {
        const z = data.indexOf(0);
        if (z > 0 && latin(data.subarray(0, z)) === RECIPE_TAG) {
          const compressed = data[z + 1] === 1;
          let p = z + 3;
          p = data.indexOf(0, p) + 1;      // language
          p = data.indexOf(0, p) + 1;      // translated keyword
          if (p <= 0) return null;
          let text = data.subarray(p);
          if (compressed) text = new Uint8Array(await new Response(new Blob([text]).stream().pipeThrough(new DecompressionStream("deflate"))).arrayBuffer());
          return parse(new TextDecoder().decode(text));
        }
      }
      at += 12 + len;
    }
  } catch (e) {}
  return null;
}

// ------------------------------------------------------------ save files
// A save file is a File from this device, or the path of one in the file
// browser. Opening it can tell us its source has moved: then the user
// finds the source in the file browser, or uses only the settings.
const SAVE_FILE = /\.primitive$/i;
async function openSave(save, { source = null, settings = false, asked = false } = {}) {
  if (st && st.render) { tell("A render is running. Cancel it first."); return; }
  if (!asked && st && st.has_work && !(await ask("open-save", "Opening this save file replaces the work on screen. Go on?"))) return;
  const isPath = typeof save === "string";
  const label = isPath ? save.split(/[\\/]/).pop() : save.name;
  let q = source ? "source=" + encodeURIComponent(source) : "";
  if (settings) q += (q ? "&" : "") + "settings=1";
  let status, body;
  showBusy(`Opening ${label}`, 0);
  try {
    if (isPath) {
      const r = await fetch("/open?path=" + encodeURIComponent(save) + (q ? "&" + q : ""), { method: "POST" });
      [status, body] = [r.status, await r.text()];
    } else {
      [status, body] = await new Promise((done) => {
        const xhr = new XMLHttpRequest();
        xhr.open("POST", "/save-open" + (q ? "?" + q : ""));
        xhr.upload.onprogress = (e) => { if (e.lengthComputable) showBusy(`Opening ${label}`, e.loaded / e.total); };
        xhr.onload = () => done([xhr.status, xhr.responseText]);
        xhr.onerror = () => done([0, "Could not send the file."]);
        xhr.send(save);
      });
    }
  } finally {
    hideBusy();
  }
  if (status !== 200) { tell(body || `Could not open ${label}.`); return; }
  let d = {};
  try { d = JSON.parse(body); } catch (e) {}
  lastEdit = 0;
  if (d.missing != null) {
    const what = d.kind === "video" ? "video" : "picture";
    const pick = await popup(`The ${what} ${d.missing} is not where it was when you saved, or it was uploaded. Find it on this computer, or use only the settings of the save?`,
      { ok: "Find it", alt: "Use only the settings" });
    if (pick === "yes") openBrowser({ save, name: d.missing });
    else if (pick === "alt") openSave(save, { settings: true, asked: true });
  } else if (d.settings_only === "sequence") {
    tell("This save was made on an image sequence. Image sequences are not kept in a save file, so only the settings were loaded.");
  }
}

// Sends a recipe (or older settings file) to the program, after asking
// about the work it would throw away.
async function loadRecipe(data, name) {
  const isRecipe = typeof data.recipe === "number";
  const cur = readSettings();
  const s = data.settings || {};
  const known = Object.keys(cur).filter((k) => k in s && typeof s[k] === typeof cur[k]);
  if (!known.length && !isRecipe) { tell(`${name} has no settings in it.`); return; }
  const hasKeys = isRecipe && data.keys && Object.keys(data.keys).length > 0;
  const image = st && st.kind === "image";
  if (isRecipe && image && st.has_work &&
      !(await ask("recipe-restart", "Loading this recipe starts the picture again and throws away its shapes. Go on?"))) return;
  if (st && st.kind === "video" && "build_on" in s && s.build_on !== cur.build_on && st.has_work &&
      !(await ask("switch-frames", "These settings switch how video frames are built. That clears the shapes of every frame. Go on?"))) return;
  lastEdit = 0;
  send({ t: "recipe", r: data });
  if (hasKeys && !image) tell("This recipe has keyframes. Keyframes work only on a still picture, so they were left out. Open a still picture and load the recipe again to use them.");
}

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
  if (SAVE_FILE.test(file.name)) return openSave(file);
  // A PNG or SVG exported by this program holds the recipe it was made with.
  // Opening it asks whether to use that recipe too. (Load in the panel
  // puts a recipe on the current picture without opening the file.)
  const recipe = await recipeIn(file);
  let withRecipe = false;
  if (recipe) {
    if (/\.svg$/i.test(file.name) || file.type === "image/svg+xml") {
      // The program cannot open an SVG as a picture, so only the recipe is offered.
      if (await popup(`${file.name} holds the recipe it was made with (settings, keyframes and painted area). An SVG cannot be opened as a picture. Use its recipe on the current picture?`, { ok: "Use the recipe" })) loadRecipe(recipe, file.name);
      return;
    }
    const pick = await popup(`${file.name} holds the recipe it was made with (settings, keyframes and painted area). Open the picture with its recipe, or without it?`,
      { ok: "Open with recipe", alt: "Open without recipe" });
    if (pick === "no") return;
    withRecipe = pick === "yes";
  }
  if (st && st.has_work && st.kind === "video" && !(await ask("load-file", "Loading a new file throws away the work on this video. Go on?"))) return;
  const xhr = new XMLHttpRequest();
  xhr.open("POST", "/upload?name=" + encodeURIComponent(file.name));
  xhr.upload.onprogress = (e) => {
    if (e.lengthComputable) showBusy(`Uploading ${file.name}`, e.loaded / e.total);
  };
  xhr.onload = () => {
    hideBusy();
    if (xhr.status !== 200) { localError = xhr.responseText || "Upload failed."; render(); return; }
    // The program loads the picture before it answers, so the recipe lands on the new picture.
    if (withRecipe) { lastEdit = 0; send({ t: "recipe", r: recipe }); }
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

// ---------------------------------------------------------- file browser
// Lists the home folder of the computer the program runs on (see `browse`
// in main.rs). Opening a file from here lets the program know where the
// file really is; an upload only gives it a copy.
let browseDir = localStorage.getItem("browse-dir") || "";
let browseShown = "";
async function showFolder(dir) {
  const list = $("browser-list");
  let r = await fetch("/browse" + (dir ? "?dir=" + encodeURIComponent(dir) : ""));
  if (!r.ok && dir) { dir = ""; r = await fetch("/browse"); }   // the folder is gone: go home
  list.textContent = "";
  if (!r.ok) { $("browser-where").textContent = "The folders cannot be shown."; return; }
  const d = await r.json();
  browseDir = d.dir;
  browseShown = d.shown;
  localStorage.setItem("browse-dir", d.dir);
  $("browser-where").textContent = d.shown;
  $("browser-up").disabled = !d.up;
  $("browser-up").onclick = () => showFolder(d.up);
  const row = (cls, name, size, act) => {
    const li = document.createElement("li");
    li.className = cls;
    li.tabIndex = 0;
    const n = document.createElement("span");
    n.className = "n";
    n.textContent = name;
    li.append(n);
    if (size != null) {
      const s = document.createElement("span");
      s.className = "s";
      s.textContent = size < 1e6 ? `${Math.max(1, Math.round(size / 1e3))} KB` : `${(size / 1e6).toFixed(1)} MB`;
      li.append(s);
    }
    li.onclick = act;
    li.onkeydown = (e) => { if (e.key === "Enter") act(); };
    list.append(li);
  };
  for (const f of d.folders) row("folder", f.name, null, () => showFolder(f.path));
  for (const f of d.files) {
    if (finding && SAVE_FILE.test(f.name)) continue;
    // Picking a folder: the files are shown so the user sees what is there.
    if (picking) row("file dim", f.name, f.size, () => {});
    else row("file", f.name, f.size, () => openPath(f.path, f.name));
  }
  if (!d.folders.length && !d.files.length) row("note", "No pictures, videos or folders here.", null, () => {});
  list.scrollTop = 0;
}
// `find`: { save, name } when the user looks for the moved source of a
// save file. Picking a file then opens the save with that file.
let finding = null;
function openBrowser(find = null) {
  finding = find;
  browserMode(false);
  $("browser-find").hidden = !find;
  $("browser-find").textContent = find ? `Find ${find.name}, then click it.` : "";
  $("browser-upload").hidden = !!find;
  $("browser").showModal();
  showFolder(browseDir);
}
// Shows the buttons for opening a file, or for picking a folder.
function browserMode(pick) {
  for (const id of ["browser-download", "browser-here", "browser-mkdir", "browser-remember-row"]) $(id).hidden = !pick;
  $("browser-mkdir-row").hidden = true;
  $("browser-upload").hidden = pick;
  $("browser-close").className = pick ? "" : "primary";
}
$("browser").addEventListener("close", () => {
  setTimeout(() => { finding = null; }, 0);
  if (picking) { const p = picking; picking = null; p.done(null); }   // Esc or Cancel
});
$("browser-close").onclick = () => $("browser").close();
$("browser-upload").onclick = () => { $("browser").close(); $("file").click(); };

// ------------------------------------------------------- output folders
// Save, Export and Render each write into their own folder on the computer
// the program runs on. With no folder set, each one opens the file browser
// to pick a folder; "Always use this folder" makes it the set folder for
// that button. "Download to this device" sends the file to the browser
// instead, for a phone or laptop. Set folders are kept in this browser.
const FOLDER_WORDS = { save: "saves", export: "exports", render: "renders" };
const HERE_WORDS = { save: "Save here", export: "Export here", render: "Render here" };
let picking = null;          // { kind, done } while the file browser picks a folder
function setFolder(kind) {
  try { return JSON.parse(localStorage.getItem("folder-" + kind) || "null"); } catch { return null; }
}
// Resolves to { path, shown }, "download", or null when the user cancelled.
function pickFolder(kind, remember = false) {
  return new Promise((done) => {
    finding = null;
    picking = { kind, done };
    browserMode(true);
    $("browser-find").hidden = false;
    $("browser-find").textContent = `Pick the folder for ${FOLDER_WORDS[kind]}: open it, then press ${HERE_WORDS[kind]}.`;
    $("browser-here").textContent = HERE_WORDS[kind];
    $("browser-remember").checked = remember;
    $("browser-remember-text").textContent = `Always use this folder for ${FOLDER_WORDS[kind]}`;
    $("browser").showModal();
    showFolder(browseDir);
  });
}
function endPick(result) {
  const p = picking;
  picking = null;
  $("browser").close();
  if (p) p.done(result);
}
$("browser-here").onclick = () => {
  if (!picking) return;
  const f = { path: browseDir, shown: browseShown };
  if ($("browser-remember").checked) { localStorage.setItem("folder-" + picking.kind, JSON.stringify(f)); showFolders(); }
  endPick(f);
};
$("browser-download").onclick = () => endPick("download");
$("browser-mkdir").onclick = () => {
  $("browser-mkdir-row").hidden = !$("browser-mkdir-row").hidden;
  if (!$("browser-mkdir-row").hidden) { $("browser-mkdir-name").value = ""; $("browser-mkdir-name").focus(); }
};
async function makeFolder() {
  const name = $("browser-mkdir-name").value.trim();
  if (!name) return;
  const r = await fetch(`/mkdir?dir=${encodeURIComponent(browseDir)}&name=${encodeURIComponent(name)}`, { method: "POST" });
  if (!r.ok) { tell(await r.text()); return; }
  $("browser-mkdir-row").hidden = true;
  showFolder((await r.json()).path);
}
$("browser-mkdir-ok").onclick = makeFolder;
$("browser-mkdir-name").addEventListener("keydown", (e) => {
  if (e.key === "Enter") { e.preventDefault(); makeFolder(); }
  if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); $("browser-mkdir-row").hidden = true; }
});
// The set folder of Save, Export or Render, checked that it is still
// there; else the user picks one.
async function folderFor(kind) {
  const f = setFolder(kind);
  if (f) {
    const r = await fetch("/browse?dir=" + encodeURIComponent(f.path));
    if (r.ok) return f;
    localStorage.removeItem("folder-" + kind);
    showFolders();
    await tell(`The folder for ${FOLDER_WORDS[kind]}, ${f.shown}, is not there any more. Pick another one.`);
  }
  return pickFolder(kind);
}
// Asks the program to write a file into a picked folder. When a file of
// that name is there, the user picks Overwrite, Save as new or Cancel.
// Gives where it was saved, or null when the user cancelled.
async function writeInto(url, dir) {
  let replace = "";
  for (;;) {
    const r = await fetch(`${url}&dir=${encodeURIComponent(dir.path)}${replace}`);
    if (r.status === 409) {
      const { exists } = await r.json();
      hideBusy();
      const pick = await popup(`${exists} is already in ${dir.shown}. Write over it, or save the new file next to it with a number added?`,
        { ok: "Save as new", alt: "Overwrite" });
      if (pick === "no") return null;
      replace = pick === "alt" ? "&replace=1" : "&replace=0";
      continue;
    }
    if (!r.ok) throw new Error(await r.text());
    return (await r.json()).saved;
  }
}
let noteTimer = 0;
function savedNote(text) {
  $("saved-note").textContent = text;
  clearTimeout(noteTimer);
  noteTimer = setTimeout(() => { $("saved-note").textContent = ""; }, 10000);
}
// The Folders list in the Export section.
function showFolders() {
  const box = $("folders");
  box.textContent = "";
  for (const kind of ["save", "export", "render"]) {
    const f = setFolder(kind);
    const row = document.createElement("div");
    row.className = "folder-row";
    const where = document.createElement("span");
    where.className = "where hint";
    where.textContent = `${FOLDER_WORDS[kind][0].toUpperCase()}${FOLDER_WORDS[kind].slice(1)}: ${f ? f.shown : "ask each time"}`;
    const change = document.createElement("button");
    change.textContent = f ? "Change" : "Set";
    change.title = `Pick the folder that ${FOLDER_WORDS[kind]} always go to`;
    change.onclick = async () => { await pickFolder(kind, true); showFolders(); };
    row.append(where, change);
    if (f) {
      const forget = document.createElement("button");
      forget.textContent = "Ask";
      forget.title = `Forget this folder: ${FOLDER_WORDS[kind]} ask for a folder each time`;
      forget.onclick = () => { localStorage.removeItem("folder-" + kind); showFolders(); };
      row.append(forget);
    }
    box.append(row);
  }
}
showFolders();
async function openPath(path, name) {
  const find = finding;
  $("browser").close();
  if (find) return openSave(find.save, { source: path, asked: true });
  if (SAVE_FILE.test(name)) return openSave(path);
  if (st && st.has_work && st.kind === "video" && !(await ask("load-file", "Loading a new file throws away the work on this video. Go on?"))) return;
  showBusy(`Opening ${name}`, 0);
  const r = await fetch("/open?path=" + encodeURIComponent(path), { method: "POST" });
  hideBusy();
  if (!r.ok) { localError = (await r.text()) || `Could not open ${name}.`; render(); }
}

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

$("pick-browse").onclick = () => openBrowser();
$("pick").onclick = () => $("file").click();
$("pick-folder").onclick = () => $("folder").click();
$("open").onclick = () => openBrowser();
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
  if (st.kind === "video") {
    const dir = await folderFor("render");
    if (!dir) return;
    // Download: render into the default folder and download it at the end.
    myRender = dir === "download";
    send(myRender ? { t: "render" } : { t: "render", dir: dir.path });
    return;
  }
  const fmt = $("export_fmt").value;
  const dir = await folderFor("export");
  if (!dir) return;
  exporting = true;
  $("render").disabled = true;
  $("render").textContent = ["mp4", "webm", "gif"].includes(fmt) ? "Making video…" : "Exporting…";
  try {
    if (dir !== "download") {
      const where = await writeInto(`/export?fmt=${fmt}`, dir);
      if (where) savedNote(`Exported ${where}`);
      exporting = false;
      render();
      return;
    }
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
  const dir = await folderFor("export");
  if (!dir) return;
  savingFrame = true;
  render();
  try {
    if (dir !== "download") {
      const where = await writeInto(`/export?fmt=${fmt}`, dir);
      if (where) savedNote(`Saved ${where}`);
      savingFrame = false;
      render();
      return;
    }
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
  if (st && st.kind === "image" && st.ready && !st.render) { moveHead(shapeAt(e)); return; }
  if (!st || !st.video || st.render) return;
  const r = tl.getBoundingClientRect();
  const n = st.video.frames;
  const f = Math.max(0, Math.min(n - 1, Math.floor((e.clientX - r.left) / r.width * n)));
  const now = Date.now();
  if (now - lastSeek < 40 && e.type === "pointermove") return;
  lastSeek = now;
  send({ t: "seek", frame: f });
}
// Still picture: the top lane moves the playhead, the bottom lane holds the
// keyframe diamonds. The two never overlap.
const HEAD_LANE = 18;
function inKeyLane(e) { return st && st.kind === "image" && e.clientY - tl.getBoundingClientRect().top >= HEAD_LANE; }
function keyNear(e) {
  const x = e.clientX - tl.getBoundingClientRect().left;
  let best, bestD = 9;
  for (const at of keyShapes()) { const d = Math.abs(shapeX(at) - x); if (d < bestD) { best = at; bestD = d; } }
  return best;
}
tl.addEventListener("pointerdown", (e) => {
  tl.setPointerCapture(e.pointerId);
  if (inKeyLane(e)) {
    // Pressing on a diamond picks it up. A drag moves it. The playhead
    // stays where it is. Empty space here does nothing.
    const near = keyNear(e);
    if (near !== undefined) keyDrag = { from: near, to: near, x: e.clientX, moved: false };
    return;
  }
  seeking = true;
  seekAt(e);
});
tl.addEventListener("pointermove", (e) => {
  if (!keyDrag && !seeking && st && st.kind === "image")
    tl.style.cursor = inKeyLane(e) ? (keyNear(e) !== undefined ? "grab" : "default") : "ew-resize";
  if (keyDrag) {
    keyDrag.moved = keyDrag.moved || Math.abs(e.clientX - keyDrag.x) > 3;
    if (keyDrag.moved) { keyDrag.to = Math.min(shapeAt(e), st.settings.count - 1); drawTimeline(); }
  } else if (seeking) seekAt(e);
});
tl.addEventListener("pointerup", (e) => {
  if (keyDrag) {
    if (keyDrag.moved && keyDrag.to !== keyDrag.from) send({ t: "key_move", from: keyDrag.from, to: keyDrag.to });
    keyDrag = null;
  } else if (seeking) seekAt(e);
  seeking = false;
});

// ------------------------------------------------------------- keyframes
// Works like Blender: a dot beside each setting makes a keyframe at the
// playhead. Yellow field = a key on this shape, green = keys elsewhere,
// orange = changed here but not keyed yet.

const KEYABLE = ["mode", "alpha", "repeat", "c_angle", "c_angle_deg", "c_angle_step", "c_x", "c_y", "c_min_size", "c_max_size", "c_shrink",
  "c_min_ratio", "c_max_ratio", "c_grid", "c_area", "chains", "random", "age", "k"];
const keyDots = {};
let keyDrag = null;
let maskRev = null;

// The playhead follows the mouse on this page at once. The picture follows
// when the program has made it.
let localHead = null, localHeadTime = 0, headFrame = 0;
function shownHead() { return localHead === null ? st.keys.head : localHead; }
function moveHead(at) {
  localHead = at;
  localHeadTime = Date.now();
  if (headFrame) return;
  headFrame = requestAnimationFrame(() => {
    headFrame = 0;
    if (localHead === null) return;
    send({ t: "shape_view", at: localHead });
    drawTimeline();
    $("time").textContent = `${localHead} / ${st.settings.count} shapes`;
  });
}

function shapeAt(e) {
  const r = tl.getBoundingClientRect();
  return Math.max(0, Math.min(st.settings.count, Math.round((e.clientX - r.left) / r.width * st.settings.count)));
}
function shapeX(at) { return at / Math.max(1, st.settings.count) * tl.clientWidth; }
function keyHead() { return Math.min(shownHead(), Math.max(0, st.settings.count - 1)); }
function keyShapes() { return [...new Set(Object.values(st.keys.tracks).flat())].sort((a, b) => a - b); }
function keyState(f) {
  const t = st && st.keys && st.keys.tracks[f];
  return !t ? "" : t.includes(keyHead()) ? "on" : "anim";
}
function panelValue(f) { return f === "c_shrink" ? $(f).checked : $(f).value; }

function toggleKey(field) {
  send({ t: keyState(field) === "on" ? "key_del" : "key_set", field, at: keyHead() });
}

function addKeyDot(field, host, inPair) {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "key-dot" + (inPair ? " in-pair" : "");
  b.title = "Keyframe this setting at the playhead";
  b.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    if (!st || !st.ready) return;
    toggleKey(field);
  });
  host.append(b);
  keyDots[field] = b;
}
for (const f of KEYABLE) {
  const row = $(f).closest("label");
  row.classList.add("keyed-row");
  addKeyDot(f, row, false);
  // Changing a value on its own keyframe updates that keyframe.
  const update = () => { if (keyState(f) === "on") send({ t: "key_set", field: f, at: keyHead() }); };
  $(f).addEventListener("change", update);
  const range = $(f + "-range");
  if (range) range.addEventListener("change", update);
}
addKeyDot("mask", $("mask-row").querySelector(".pair"), true);

function drawKeyDots() {
  document.body.classList.toggle("has-keys", st.kind === "image" && !!st.ready);
  for (const f of Object.keys(keyDots)) {
    let state = keyState(f);
    if (state === "anim" && f !== "mask" && String(Number(panelValue(f)) || panelValue(f)) !== String(Number(st.keys.values[f]) || st.keys.values[f])) state = "changed";
    keyDots[f].classList.toggle("on", state === "on");
    keyDots[f].classList.toggle("anim", state === "anim");
    keyDots[f].classList.toggle("changed", state === "changed");
    if (f === "mask") continue;
    $(f).classList.toggle("key-on", state === "on");
    $(f).classList.toggle("key-anim", state === "anim");
    $(f).classList.toggle("key-changed", state === "changed");
  }
}

// A keyframe changed the painted area: show the new one on this page.
function syncKeyMask() {
  const rev = st.keys.mask_rev;
  if (maskRev === rev) return;
  const first = maskRev === null;
  maskRev = rev;
  if (first) return;
  fetch("/mask").then((r) => r.json()).then((m) => {
    maskData = null;
    if (m.w > 0) {
      maskData = document.createElement("canvas");
      maskData.width = m.w; maskData.height = m.h;
      const g = maskData.getContext("2d");
      const img = g.createImageData(m.w, m.h);
      for (let i = 0; i < m.bits.length; i++) img.data[i * 4 + 3] = m.bits[i] === "1" ? 255 : 0;
      g.putImageData(img, 0, 0);
    }
    $("mask-invert").checked = false;
    drawOverlay();
  }).catch(() => {});
}

function drawShapeTimeline() {
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(tl.clientWidth * dpr)), h = Math.round(46 * dpr);
  if (tl.width !== w || tl.height !== h) { tl.width = w; tl.height = h; }
  const g = tl.getContext("2d");
  const count = Math.max(1, st.settings.count);
  const xOf = (at) => at / count * w;
  const lane = Math.round(HEAD_LANE * dpr);
  // Top lane (playhead) is lighter than the bottom lane (keyframes).
  g.fillStyle = "#2a2a2a";
  g.fillRect(0, 0, w, lane);
  g.fillStyle = "#161616";
  g.fillRect(0, lane, w, h - lane);
  g.fillStyle = "#3a3a3a";
  g.fillRect(0, lane, w, Math.max(1, Math.round(dpr)));
  g.fillStyle = "#3fa34d";
  g.fillRect(0, h - Math.round(4 * dpr), xOf(Math.min(st.shapes, count)), Math.round(4 * dpr));
  const head = shownHead();
  const y = Math.round((HEAD_LANE + (46 - HEAD_LANE - 4) / 2) * dpr), r = 6 * dpr;
  for (let at of keyShapes()) {
    if (keyDrag && keyDrag.from === at) at = keyDrag.to;
    const x = xOf(at);
    g.fillStyle = at === head ? "#ffffff" : "#e8b23a";
    g.beginPath();
    g.moveTo(x, y - r); g.lineTo(x + r, y); g.lineTo(x, y + r); g.lineTo(x - r, y);
    g.closePath();
    g.fill();
  }
  // The playhead: a blue flag with the shape number.
  const px = xOf(head), label = String(head);
  g.font = `${Math.round(11 * dpr)}px monospace`;
  const fw = g.measureText(label).width + 8 * dpr, fh = lane;
  const fx = Math.max(0, Math.min(w - fw, px - fw / 2));
  g.fillStyle = "#237bd3";
  g.fillRect(Math.round(px - dpr / 2), 0, Math.max(1, Math.round(dpr)), h);
  g.fillRect(fx, 0, fw, fh);
  g.fillStyle = "#ffffff";
  g.textBaseline = "middle";
  g.fillText(label, fx + 4 * dpr, fh / 2 + dpr / 2);
}

// Still picture: left / right move one shape, up jumps to the last shape,
// down to the first.
document.addEventListener("keydown", (e) => {
  if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) return;
  if (!st || st.kind !== "image" || !st.ready || st.render || e.ctrlKey || e.altKey || e.metaKey || $("ask").open) return;
  if (e.target.closest && e.target.closest("input:not([type=checkbox]), select, textarea")) return;
  e.preventDefault();
  const head = shownHead();
  const at = e.key === "ArrowUp" ? st.settings.count : e.key === "ArrowDown" ? 0 : head + (e.key === "ArrowRight" ? 1 : -1);
  moveHead(Math.max(0, Math.min(st.settings.count, at)));
});

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
  if (st.kind === "image") return drawShapeTimeline();
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
  // The overlay follows the picture by itself (see the ResizeObserver).
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

  const stillLine = st.kind === "image" && !!st.ready;
  if (localHead !== null && !seeking && (!stillLine || st.keys.head === localHead || Date.now() - localHeadTime > 1000)) localHead = null;
  $("timeline-row").hidden = !(isVideo || stillLine);
  tl.classList.toggle("still", stillLine);
  if (!stillLine) tl.style.cursor = "";
  drawKeyDots();
  syncKeyMask();
  if (stillLine) {
    drawTimeline();
    $("time").textContent = `${shownHead()} / ${s.count} shapes`;
  }
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

window.addEventListener("resize", () => { if (st && (st.video || (st.kind === "image" && st.ready))) drawTimeline(); placeOverlay(); });

// Drag the panel's left edge to change its width. The page remembers it.
{
  const panel = $("panel"), grip = $("panel-grip"), KEY = "panel-width";
  const setWidth = (px) => {
    const w = Math.round(Math.max(260, Math.min(window.innerWidth * 0.6, px)));
    panel.style.width = w + "px";
    return w;
  };
  const saved = Number(localStorage.getItem(KEY));
  if (saved) setWidth(saved);
  let drag = null;
  grip.addEventListener("pointerdown", (e) => {
    e.preventDefault();
    grip.setPointerCapture(e.pointerId);
    grip.classList.add("drag");
    drag = { x: e.clientX, w: panel.getBoundingClientRect().width };
  });
  grip.addEventListener("pointermove", (e) => {
    if (!drag) return;
    setWidth(drag.w + drag.x - e.clientX);
    window.dispatchEvent(new Event("resize"));
  });
  grip.addEventListener("pointerup", () => {
    if (!drag) return;
    drag = null;
    grip.classList.remove("drag");
    localStorage.setItem(KEY, Math.round(panel.getBoundingClientRect().width));
  });
  grip.addEventListener("dblclick", () => {
    panel.style.width = "";
    localStorage.removeItem(KEY);
    window.dispatchEvent(new Event("resize"));
  });
}
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
  const show = !pic.hidden && !!picBlob;
  overlay.style.display = show ? "block" : "none";
  if (!show) return;
  const v = view.getBoundingClientRect(), r = pic.getBoundingClientRect();
  overlay.style.left = (r.left - v.left) + "px";
  overlay.style.top = (r.top - v.top) + "px";
  overlay.style.width = r.width + "px";
  overlay.style.height = r.height + "px";
  // A picture of another shape: the painted area no longer fits it.
  if (maskData && Math.abs(maskData.width / maskData.height - pic.width / pic.height) > 0.03) {
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
  if (keyState("mask") === "on") send({ t: "key_set", field: "mask", at: keyHead() });
}

function overlayPos(e) {
  const r = overlay.getBoundingClientRect();
  return [(e.clientX - r.left) / r.width, (e.clientY - r.top) / r.height];
}

function paintAt(e) {
  const pic = $("picture");
  if (!maskData) {
    const a = pic.width / pic.height;
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
// Measuring the picture on every status message made the browser redo the
// page layout each time. Now the overlay moves only when a size changes.
const picSizeWatch = new ResizeObserver(() => placeOverlay());
picSizeWatch.observe($("picture"));
picSizeWatch.observe(view);
for (const k of ["c_x", "c_y"]) $(k).addEventListener("input", drawOverlay);
showRuleRows();

// ------------------------------------------------------- right-click menu
// Right-click on a setting opens this menu. Shift + right-click still opens
// the browser's own menu.

const MENU_FIELDS = ALL.concat(["stop_match", "seed", "realtime", "loop_video", "c_shrink"]);
// The second control of a setting: a slider or a color picker.
const PARTNER = { alpha: "alpha-range", c_angle_deg: "c_angle_deg-range", work_size: "work-range", background: "background-pick" };
const fieldMenu = $("field-menu");
let copiedText = "";         // used when the browser blocks the clipboard

// The setting under the mouse, "mask" for the painted area, or null.
function menuField(target) {
  const partner = Object.keys(PARTNER).find((f) => PARTNER[f] === target.id);
  if (partner) return partner;
  if (MENU_FIELDS.includes(target.id)) return target.id;
  if (target.closest("#mask-row .pair")) return "mask";
  const label = target.closest("label");
  const inside = label ? [...label.querySelectorAll("input, select")].find((el) => MENU_FIELDS.includes(el.id)) : null;
  return inside ? inside.id : null;
}

function shownDefault(f) { return f === "stop_match" ? (defaults[f] > 0 ? String(defaults[f]) : "") : defaults[f]; }
function atDefault(f) {
  const el = $(f), d = shownDefault(f);
  if (f === "seed") return !$("seed_lock").checked;
  if (el.type === "checkbox") return el.checked === !!d;
  if (el.type === "number") return el.value === "" ? d === "" : Number(el.value) === Number(d);
  return el.value === String(d);
}
function canReset(f) {
  if (!defaults || !(f in defaults) || atDefault(f)) return false;
  const el = $(f);
  return el.tagName !== "SELECT" || [...el.options].some((o) => o.value === String(defaults[f]));
}

// Puts a value in a setting, the same way as a change by hand.
function setField(f, value) {
  const el = $(f);
  if (el.type === "checkbox") el.checked = !!value;
  else el.value = value;
  const partner = PARTNER[f] && $(PARTNER[f]);
  if (partner && (partner.type !== "color" || /^#[0-9a-fA-F]{6}$/.test(value))) partner.value = value;
  el.dispatchEvent(new Event("change", { bubbles: true }));
  showRuleRows();
}
function resetField(f) {
  if (f !== "seed") { setField(f, shownDefault(f)); return; }
  $("seed_lock").checked = false;
  $("seed_lock").dispatchEvent(new Event("change", { bubbles: true }));
}

async function clearKeys(f) {
  const first = Math.min(...st.keys.tracks[f]);
  if (st.shapes > first && !(await ask("clear-keys", `Clearing these keyframes keeps the first ${first} shapes and removes the rest.`, "Clear"))) return;
  // Last key first: the final delete removes every shape made with the old keys.
  const keys = st.keys.tracks[f] ? [...st.keys.tracks[f]].sort((a, b) => b - a) : [];
  for (const at of keys) send({ t: "key_del", field: f, at });
}

async function copyField(f) {
  copiedText = $(f).value;
  try {
    await navigator.clipboard.writeText(copiedText);
  } catch (e) {
    // The clipboard needs a secure page. Another device on the network does
    // not get one, so copy the older way.
    const box = document.createElement("textarea");
    box.value = copiedText;
    document.body.append(box);
    box.select();
    document.execCommand("copy");
    box.remove();
  }
}
async function pasteField(f) {
  let text = copiedText;
  try { text = await navigator.clipboard.readText(); } catch (e) { /* use the copied value */ }
  text = text.trim();
  if (!text) { tell("Nothing to paste. Click in the setting and press Ctrl+V."); return; }
  const el = $(f), old = el.value;
  el.value = text;
  // A number field or a list gives an empty value for text it cannot use.
  const usable = el.value;
  el.value = old;
  if (!usable) { tell(`"${text}" does not fit this setting.`); return; }
  setField(f, usable);
}

function menuItems(f) {
  const groups = [];
  const el = f === "mask" ? null : $(f);
  const locked = !!el && el.matches(":disabled");
  if (el) groups.push([["Reset to default", () => resetField(f), locked || !canReset(f)]]);
  if ((f === "mask" || KEYABLE.includes(f)) && st && st.kind === "image" && st.ready) {
    groups.push([
      [keyState(f) === "on" ? "Delete keyframe" : "Insert keyframe", () => toggleKey(f), !!st.render],
      ["Clear keyframes", () => clearKeys(f), !!st.render || !keyState(f)],
    ]);
    // Only number settings that change smoothly have this switch.
    const smooth = st.keys.smooth && st.keys.smooth[f];
    if (smooth !== undefined) {
      groups[groups.length - 1].push([smooth ? "Smooth change: on" : "Smooth change: off",
        () => send({ t: "key_smooth", field: f, on: !smooth }), !!st.render]);
    }
  }
  if (el && el.type !== "checkbox") {
    groups.push([["Copy", () => copyField(f), !el.value], ["Paste", () => pasteField(f), locked]]);
  }
  return groups;
}

function closeFieldMenu() { fieldMenu.hidden = true; }
function openFieldMenu(groups, x, y) {
  fieldMenu.innerHTML = "";
  groups.forEach((group, i) => {
    if (i) fieldMenu.append(document.createElement("hr"));
    for (const [label, run, off] of group) {
      const b = document.createElement("button");
      b.type = "button";
      b.setAttribute("role", "menuitem");
      b.textContent = label;
      b.disabled = off;
      b.onclick = () => { closeFieldMenu(); run(); };
      fieldMenu.append(b);
    }
  });
  fieldMenu.hidden = false;
  fieldMenu.style.left = Math.max(4, Math.min(x, innerWidth - fieldMenu.offsetWidth - 4)) + "px";
  fieldMenu.style.top = Math.max(4, Math.min(y, innerHeight - fieldMenu.offsetHeight - 4)) + "px";
  fieldMenu.focus({ preventScroll: true });
}

$("panel").addEventListener("contextmenu", (e) => {
  closeFieldMenu();
  if (e.shiftKey) return;
  const f = menuField(e.target);
  if (!f) return;
  // A value still being typed goes in first.
  if (document.activeElement && document.activeElement.matches("input, select")) document.activeElement.blur();
  const groups = menuItems(f);
  if (!groups.length) return;
  e.preventDefault();
  openFieldMenu(groups, e.clientX, e.clientY);
});
fieldMenu.addEventListener("contextmenu", (e) => e.preventDefault());
fieldMenu.addEventListener("keydown", (e) => {
  e.stopPropagation();
  if (e.key === "Escape" || e.key === "Tab") { e.preventDefault(); closeFieldMenu(); return; }
  if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
  e.preventDefault();
  const on = [...fieldMenu.querySelectorAll("button:not(:disabled)")];
  if (!on.length) return;
  const i = on.indexOf(document.activeElement);
  on[e.key === "ArrowDown" ? (i + 1) % on.length : (i <= 0 ? on.length : i) - 1].focus();
});
document.addEventListener("pointerdown", (e) => { if (!fieldMenu.contains(e.target)) closeFieldMenu(); }, true);
document.addEventListener("scroll", closeFieldMenu, true);
window.addEventListener("resize", closeFieldMenu);
window.addEventListener("blur", closeFieldMenu);
