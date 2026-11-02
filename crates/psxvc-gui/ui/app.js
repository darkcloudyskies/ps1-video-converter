"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const dialog = window.__TAURI__.dialog;

const $ = (id) => document.getElementById(id);

/** How much the difference view brightens what it shows. */
const DIFF_GAIN = 16;

const PRESETS = {
  h264: { audio: "AAC", hint: "8-bit, for players that can't handle anything newer." },
  av1: { audio: "FLAC", hint: "10-bit with HQ, and smaller than H.264." },
  archive: { audio: "FLAC", hint: "Lossless, for keeping. Many players can't show it, including the Windows default." },
};

const state = {
  discs: [],
  disc: null,
  stream: null,
  frames: [],
  position: 0,
  view: "hq",
  /** Streams ticked for export, by disc id. */
  ticked: new Map(),
  folder: "",
};

function ticked(disc = state.disc) {
  if (!state.ticked.has(disc.id)) state.ticked.set(disc.id, new Set());
  return state.ticked.get(disc.id);
}

// Remembered settings are only a convenience, so a failure just means defaults.
function remember(key, value) {
  try { localStorage.setItem(key, value); } catch {}
}
function recall(key) {
  try { return localStorage.getItem(key); } catch { return null; }
}

function formatLength(seconds) {
  const s = Math.round(seconds);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

function formatTime(seconds) {
  const m = Math.floor(seconds / 60);
  return `${m}:${(seconds - m * 60).toFixed(2).padStart(5, "0")}`;
}

function element(tag, className, text) {
  const e = document.createElement(tag);
  if (className) e.className = className;
  if (text !== undefined) e.textContent = text;
  return e;
}

function plural(n, word) {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

// ---- The disc browser ----

function setDiscStatus(text, warn = false) {
  const s = $("disc-status");
  s.hidden = !text;
  s.textContent = text || "";
  s.classList.toggle("warn", warn);
}

async function pickDisc() {
  const path = await dialog.open({
    title: "Open a disc image",
    filters: [{ name: "Disc images", extensions: ["cue", "bin"] }],
  });
  if (path) await openDisc(path);
}

async function openDisc(path) {
  $("open-disc").disabled = true;
  setDiscStatus("Opening…");
  try {
    const disc = await invoke("open_disc", { path });
    if (!state.discs.some((d) => d.id === disc.id)) state.discs.push(disc);
    selectDisc(disc.id);
  } catch (e) {
    setDiscStatus(String(e), true);
  } finally {
    $("open-disc").disabled = false;
  }
}

function selectDisc(id) {
  state.disc = state.discs.find((d) => d.id === id);
  const select = $("disc-select");
  select.replaceChildren(...state.discs.map((d) => {
    const o = element("option", "", d.name);
    o.value = d.id;
    return o;
  }));
  select.value = id;
  select.hidden = false;
  $("browser-empty").hidden = true;
  $("stream-tools").hidden = false;

  const { edc_errors: bad, streams } = state.disc;
  setDiscStatus(bad > 0 ? `${plural(bad, "sector")} failed the EDC check, so this rip may be damaged.` : "", bad > 0);
  $("stream-count").textContent = plural(streams.length, "stream");
  if (streams.length === 0) setDiscStatus("No video streams found on this disc.", true);

  renderStreams();
  if (streams.length > 0) selectStream(streams[0].id);
  else clearViewer();
  updateAddButton();
}

function renderStreams() {
  const chosen = ticked();
  $("streams").replaceChildren(...state.disc.streams.map((s) => {
    const row = element("li", "stream");
    row.dataset.id = s.id;
    const box = element("input");
    box.type = "checkbox";
    box.checked = chosen.has(s.id);
    box.setAttribute("aria-label", `Export stream ${s.id}`);
    box.addEventListener("click", (e) => e.stopPropagation());
    box.addEventListener("change", () => {
      if (box.checked) chosen.add(s.id);
      else chosen.delete(s.id);
      updateAddButton();
    });
    const text = element("div");
    text.append(element("div", "stream-name", `${s.id}  ${s.file || "(no file)"}`));
    const audio = s.audio.length ? s.audio.join(", ") : "no audio";
    text.append(element("div", "stream-meta",
      `${s.width}×${s.height} · ${formatLength(s.seconds)} · ${s.fps.toFixed(3)} fps${s.constant_rate ? "" : " VFR"} · ${s.variant}`));
    text.append(element("div", "stream-meta", audio));
    if (s.incomplete_frames > 0) {
      text.append(element("div", "stream-warn", `${plural(s.incomplete_frames, "incomplete frame")}`));
    }
    row.append(box, text);
    row.addEventListener("click", () => selectStream(s.id));
    return row;
  }));
  markSelectedStream();
}

function markSelectedStream() {
  for (const row of $("streams").children) {
    row.classList.toggle("selected", Number(row.dataset.id) === state.stream?.id);
  }
}

// ---- The viewer ----

function clearViewer() {
  state.stream = null;
  state.frames = [];
  $("stream-title").textContent = "No stream selected";
  $("stream-detail").textContent = "";
  $("frame-label").textContent = "";
  $("frame-problem").hidden = true;
  for (const b of ["scrub", "prev-frame", "next-frame"]) $(b).disabled = true;
  for (const p of ["pane-a", "pane-b", "pane-diff"]) clearPane($(p));
}

function clearPane(pane) {
  const canvas = pane.querySelector("canvas");
  canvas.getContext("2d").clearRect(0, 0, canvas.width, canvas.height);
  pane.querySelector("figcaption").textContent = "";
}

async function selectStream(id) {
  const disc = state.disc;
  const stream = disc.streams.find((s) => s.id === id);
  state.stream = stream;
  state.frames = [];
  markSelectedStream();
  updateAddButton();
  $("stream-title").textContent = stream.file || `Stream ${stream.id}`;
  $("stream-detail").textContent = `Stream ${stream.id} · ${stream.width}×${stream.height} · ${plural(stream.frame_count, "frame")}`;
  try {
    const frames = await invoke("stream_frames", { disc: disc.id, stream: id });
    if (state.stream !== stream) return; // Another stream was picked meanwhile.
    state.frames = frames;
  } catch (e) {
    showFrameProblem(String(e));
    return;
  }
  const scrub = $("scrub");
  scrub.max = Math.max(0, state.frames.length - 1);
  scrub.value = 0;
  for (const b of ["scrub", "prev-frame", "next-frame"]) $(b).disabled = state.frames.length === 0;
  seek(0);
}

function seek(position) {
  if (state.frames.length === 0) return;
  state.position = Math.min(Math.max(position, 0), state.frames.length - 1);
  $("scrub").value = state.position;
  updateFrameLabel();
  requestFrame();
}

function updateFrameLabel() {
  const f = state.frames[state.position];
  $("frame-label").textContent =
    `Frame ${f.number} (${state.position + 1} of ${state.frames.length}) · ${formatTime(f.seconds)} · q${f.quant_scale}`;
}

function showFrameProblem(text) {
  const p = $("frame-problem");
  p.hidden = !text;
  p.textContent = text || "";
}

// Decoding takes a moment, so while dragging only the newest wanted frame gets
// decoded next. The picture keeps up as well as it can without building a backlog.
let wanted = null;
let decoding = false;

function requestFrame() {
  wanted = { disc: state.disc.id, stream: state.stream, position: state.position, view: state.view };
  if (!decoding) decodeWanted();
}

async function decodeWanted() {
  decoding = true;
  while (wanted) {
    const job = wanted;
    wanted = null;
    try {
      const images = await Promise.all(idctsFor(job.view).map((idct) => fetchFrame(job, idct)));
      if (job.stream === state.stream) showImages(job.view, images);
    } catch (e) {
      if (job.stream === state.stream) {
        for (const p of ["pane-a", "pane-b", "pane-diff"]) clearPane($(p));
        showFrameProblem(`This frame can't be shown, and an export would skip it: ${e}`);
      }
    }
  }
  decoding = false;
}

function idctsFor(view) {
  return view === "compare" ? ["accurate", "hq"] : [view];
}

async function fetchFrame(job, idct) {
  const buffer = await invoke("frame", {
    disc: job.disc,
    stream: job.stream.id,
    position: job.position,
    idct,
  });
  const head = new DataView(buffer);
  return {
    idct,
    width: head.getUint16(0, true),
    height: head.getUint16(2, true),
    missing: head.getUint16(4, true),
    badEdc: head.getUint16(6, true),
    rgba: new Uint8ClampedArray(buffer, 8),
  };
}

function showImages(view, images) {
  const a = images[0];
  drawImage($("pane-a"), a.rgba, a.width, a.height, images.length > 1 ? "Accurate: as the console's MDEC" : "");
  $("pane-b").hidden = images.length < 2;
  $("pane-diff").hidden = images.length < 2;
  $("stage").classList.toggle("compare", images.length > 1);
  if (images.length > 1) {
    const b = images[1];
    drawImage($("pane-b"), b.rgba, b.width, b.height, "HQ: shown at 8 bits");
    const diff = difference(a.rgba, b.rgba);
    drawImage($("pane-diff"), diff.rgba, a.width, a.height,
      `Difference ×${DIFF_GAIN}: mean ${diff.mean.toFixed(2)}, max ${diff.max} of 255`);
  }

  const problems = [];
  if (a.missing > 0) problems.push(`${plural(a.missing, "sector")} of this frame are missing.`);
  if (a.badEdc > 0) problems.push(`${plural(a.badEdc, "sector")} of this frame fail the EDC check, so the picture may be damaged.`);
  showFrameProblem(problems.join(" "));
}

function drawImage(pane, rgba, width, height, caption) {
  const canvas = pane.querySelector("canvas");
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  canvas.getContext("2d").putImageData(new ImageData(rgba, width, height), 0, 0);
  pane.querySelector("figcaption").textContent = caption;
}

/** The two IDCTs are hard to tell apart by eye, so this shows where they differ. */
function difference(a, b) {
  const rgba = new Uint8ClampedArray(a.length);
  let total = 0;
  let max = 0;
  for (let i = 0; i < a.length; i += 4) {
    for (let c = 0; c < 3; c++) {
      const d = Math.abs(a[i + c] - b[i + c]);
      total += d;
      if (d > max) max = d;
      rgba[i + c] = d * DIFF_GAIN;
    }
    rgba[i + 3] = 255;
  }
  return { rgba, mean: total / (a.length / 4 * 3), max };
}

function setView(view) {
  state.view = view;
  for (const b of document.querySelectorAll(".segmented button")) {
    b.setAttribute("aria-pressed", String(b.dataset.view === view));
  }
  if (view !== "compare") {
    $("pane-b").hidden = true;
    $("pane-diff").hidden = true;
    $("stage").classList.remove("compare");
  }
  if (state.frames.length > 0) requestFrame();
}

// ---- Export settings ----

function updatePreset() {
  const preset = $("preset").value;
  const info = PRESETS[preset];
  $("preset-hint").textContent = info.hint;
  const audio = $("audio");
  const previous = audio.value;
  const options = [["", `Preset's own (${info.audio})`]];
  if (preset !== "archive") options.push(["flac", "FLAC"], ["opus", "Opus"], ["aac", "AAC"]);
  audio.replaceChildren(...options.map(([value, label]) => {
    const o = element("option", "", label);
    o.value = value;
    return o;
  }));
  audio.value = options.some(([v]) => v === previous) ? previous : "";
  audio.disabled = preset === "archive";
  remember("preset", preset);
}

async function chooseFolder() {
  const folder = await dialog.open({ title: "Choose where exports go", directory: true });
  if (!folder) return false;
  setFolder(folder);
  return true;
}

function setFolder(folder) {
  state.folder = folder;
  $("folder").value = folder;
  $("folder").title = folder;
  remember("folder", folder);
}

/** The ticked streams, or else the one on screen. */
function streamsToExport() {
  if (!state.disc) return [];
  const chosen = ticked();
  if (chosen.size > 0) return state.disc.streams.filter((s) => chosen.has(s.id)).map((s) => s.id);
  return state.stream ? [state.stream.id] : [];
}

function updateAddButton() {
  const streams = streamsToExport();
  const add = $("add");
  add.disabled = streams.length === 0;
  add.textContent = streams.length === 1 ? `Add stream ${streams[0]} to queue`
    : streams.length > 1 ? `Add ${streams.length} streams to queue`
    : "Add to queue";
  if (state.disc) {
    const all = $("select-all");
    const count = ticked().size;
    all.checked = count > 0 && count === state.disc.streams.length;
    all.indeterminate = count > 0 && count < state.disc.streams.length;
  }
}

function setSettingsProblem(text) {
  const p = $("settings-problem");
  p.hidden = !text;
  p.textContent = text || "";
}

async function addToQueue(event) {
  event.preventDefault();
  setSettingsProblem("");
  if (!state.folder && !(await chooseFolder())) return;
  const audio = $("audio").value;
  try {
    await invoke("enqueue", {
      disc: state.disc.id,
      streams: streamsToExport(),
      folder: state.folder,
      settings: {
        preset: $("preset").value,
        idct: $("idct").value,
        audio: audio || null,
        cfr: $("cfr").checked,
        sar: [Number($("sar-w").value), Number($("sar-h").value)],
      },
    });
    ticked().clear();
    renderStreams();
    updateAddButton();
  } catch (e) {
    setSettingsProblem(String(e));
  }
}

// ---- The queue ----

function renderJobs(jobs) {
  $("queue-empty").hidden = jobs.length > 0;
  $("clear-finished").disabled = !jobs.some((j) => !["queued", "running"].includes(j.state));
  $("queue").replaceChildren(...jobs.map(renderJob));
}

function renderJob(job) {
  const row = element("li", "job");
  const top = element("div", "job-top");
  top.append(element("span", "job-label", job.label));
  if (job.state === "queued" || job.state === "running") {
    const cancel = element("button", "", "Cancel");
    cancel.setAttribute("aria-label", `Cancel ${job.label}`);
    cancel.addEventListener("click", () => invoke("cancel_job", { id: job.id }));
    top.append(cancel);
  }
  row.append(top);

  switch (job.state) {
    case "queued":
      row.append(element("div", "job-state", "Waiting"));
      break;
    case "running": {
      const bar = element("progress");
      bar.max = 100;
      bar.value = job.percent;
      row.append(bar, element("div", "job-state numbers", `${job.percent}%`));
      break;
    }
    case "done": {
      const done = element("div", "job-state done", `Done: ${plural(job.frames, "frame")}`);
      // The folder is already on show above, and the full path is in the tooltip.
      const path = element("div", "job-state", job.output.split(/[\\/]/).pop());
      path.title = job.output;
      row.append(done, path);
      if (job.warnings.length > 0) {
        const details = element("details");
        details.append(element("summary", "", plural(job.warnings.length, "warning")));
        const list = element("ul");
        list.append(...job.warnings.map((w) => element("li", "", w)));
        details.append(list);
        row.append(details);
      }
      break;
    }
    case "failed":
      row.append(element("div", "job-state failed", job.error));
      break;
    case "cancelled":
      row.append(element("div", "job-state", "Cancelled"));
      break;
  }
  return row;
}

// ---- Wiring ----

$("open-disc").addEventListener("click", pickDisc);
$("disc-select").addEventListener("change", (e) => selectDisc(Number(e.target.value)));
$("select-all").addEventListener("change", (e) => {
  const chosen = ticked();
  chosen.clear();
  if (e.target.checked) for (const s of state.disc.streams) chosen.add(s.id);
  renderStreams();
  updateAddButton();
});
$("scrub").addEventListener("input", (e) => seek(Number(e.target.value)));
$("prev-frame").addEventListener("click", () => seek(state.position - 1));
$("next-frame").addEventListener("click", () => seek(state.position + 1));
for (const b of document.querySelectorAll(".segmented button")) {
  b.addEventListener("click", () => setView(b.dataset.view));
}
$("preset").addEventListener("change", updatePreset);
$("choose-folder").addEventListener("click", chooseFolder);
$("settings").addEventListener("submit", addToQueue);
$("clear-finished").addEventListener("click", () => invoke("clear_finished"));

document.addEventListener("keydown", (e) => {
  // Form controls have their own use for these keys, including the scrubber.
  if (e.target.closest("input, select, textarea")) return;
  if (e.key === "ArrowLeft") seek(state.position - 1);
  if (e.key === "ArrowRight") seek(state.position + 1);
});

listen("scan-progress", (e) => setDiscStatus(`Scanning… ${e.payload.percent}%`));
listen("jobs", (e) => renderJobs(e.payload));

const savedPreset = recall("preset");
if (savedPreset in PRESETS) $("preset").value = savedPreset;
updatePreset();
const savedFolder = recall("folder");
if (savedFolder) setFolder(savedFolder);
invoke("jobs").then(renderJobs);
updateAddButton();
