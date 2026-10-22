// Voice, video (camera & screen share) and the call view.

import { $, ANDROID, avatar, call, esc, hue, key, modal, render, renderChannels, renderNav, renderUser, renderVoicePanel, roomOf, saveSettings, server, state, toast, who } from "./app.js";

const { invoke } = window.__TAURI__.core;

// ---------- voice ----------

/** Picks the microphone, speakers and camera. Works mid-call. */
export async function devicesModal() {
  const audio = await invoke("audio_devices").catch((e) => {
    toast(`Couldn't list audio devices: ${e}`, { error: true });
    return { inputs: [], outputs: [], input: null, output: null };
  });
  const cameras = await listCameras();
  const opts = (list, unnamed) => [["", "System default"], ...list.map((d, i) => [d.id, d.name || `${unnamed} ${i + 1}`])];
  // A saved device that's unplugged still shows, so saving doesn't silently forget it.
  const keep = (list, id, what) => (id && !list.some((d) => d.id === id) ? [...list, { id, name: `${what} (not connected)` }] : list);
  const camId = state.settings.camera || "";
  const root = modal({
    title: "Voice & video",
    fields: [
      { name: "input", type: "select", label: "Microphone", value: audio.input || "", options: opts(keep(audio.inputs, audio.input, "Microphone"), "Microphone") },
      { name: "output", type: "select", label: "Speakers / headphones", value: audio.output || "", options: opts(keep(audio.outputs, audio.output, "Speakers"), "Speakers") },
      { name: "camera", type: "select", label: "Camera", value: camId, options: opts(keep(cameras.map((c) => ({ id: c.deviceId, name: c.label })), camId, "Camera"), "Camera") },
    ],
    submit: "Save",
    onSubmit: async ({ input, output, camera }) => {
      if ((input || null) !== audio.input || (output || null) !== audio.output) {
        const warnings = await call("set_audio_devices", { input: input || null, output: output || null });
        for (const w of warnings) toast(w, { error: true, timeout: 8000 });
      }
      if (camera !== camId) {
        state.settings.camera = camera || null;
        saveSettings();
        await switchCamera();
      }
    },
  });
  $(".modal", root).classList.add("wide");
  callStatsPanel(root);
}

const STAT_COLORS = ["#57f287", "#5865f2", "#fee75c", "#eb459e", "#3ba55d", "#ed4245", "#9b84ee", "#f97316"];

/** Live call quality: your mic level, and per person ping, loss and more, graphed over the last minute. */
function callStatsPanel(root) {
  const box = document.createElement("div");
  box.className = "call-stats";
  box.innerHTML = `
    <h3>Call quality</h3>
    <div class="mic-meter" title="Your microphone level"><span>Mic</span><div class="meter"><div class="meter-fill"></div></div></div>
    <div class="stats-graphs">
      <figure><figcaption>Ping (ms)</figcaption><canvas data-metric="rtt"></canvas></figure>
      <figure><figcaption>Packet loss (%)</figcaption><canvas data-metric="loss"></canvas></figure>
    </div>
    <div class="stats-table"><p class="hint">Join a call to see live quality.</p></div>`;
  const form = $("form", root);
  form.insertBefore(box, $(".actions", form));
  const SAMPLES = 120; // 60 s at 2 per second
  const history = new Map(); // peer -> [{ rtt, loss }]
  let prev = null;
  const tick = async () => {
    if (!box.isConnected) return clearInterval(timer);
    const s = await invoke("call_stats").catch(() => null);
    $(".meter-fill", box).style.width = s ? `${Math.max(0, Math.min(100, ((s.input_db + 60) / 60) * 100))}%` : "0%";
    $(".meter-fill", box).classList.toggle("on", !!s?.transmitting);
    if (!s) {
      prev = null;
      return;
    }
    const now = performance.now();
    const rows = s.peers.map((p, i) => {
      const before = prev?.stats.peers.find((x) => x.peer === p.peer);
      const secs = prev ? (now - prev.at) / 1000 : 0;
      const got = before ? p.received - before.received : 0;
      const lost = before ? p.lost - before.lost : 0;
      const sample = { rtt: p.rtt_ms, loss: got + lost > 0 ? (100 * lost) / (got + lost) : null };
      const h = history.get(p.peer) || [];
      h.push(sample);
      if (h.length > SAMPLES) h.shift();
      history.set(p.peer, h);
      const kbps = before && secs ? ((p.received_bytes - before.received_bytes) * 8) / secs / 1000 : 0;
      const totalLoss = p.received + p.lost ? (100 * p.lost) / (p.received + p.lost) : 0;
      return `<tr>
        <td><span class="swatch" style="background:${STAT_COLORS[i % STAT_COLORS.length]}"></span>${esc(who(p.peer))}</td>
        <td>${p.rtt_ms == null ? "…" : `${Math.round(p.rtt_ms)} ms`}</td>
        <td class="${p.relay ? "warn" : ""}" title="${p.relay ? "Through a relay server: hole-punching hasn't connected you directly" : "Directly connected"}">${p.relay ? "relay" : "direct"}</td>
        <td class="${totalLoss > 5 ? "warn" : ""}">${totalLoss.toFixed(1)}%</td>
        <td>${p.jitter_ms.toFixed(1)} ms</td>
        <td>${Math.round(p.buffered_ms)} ms</td>
        <td>${kbps ? Math.round(kbps) : 0} kbps</td>
      </tr>`;
    });
    for (const peer of history.keys()) if (!s.peers.some((p) => p.peer === peer)) history.delete(peer);
    prev = { at: now, stats: s };
    $(".stats-table", box).innerHTML = s.peers.length
      ? `<table><thead><tr><th>Who</th><th>Ping</th><th>Path</th><th>Loss</th><th>Jitter</th><th>Buffer</th><th>In</th></tr></thead><tbody>${rows.join("")}</tbody></table>
         <p class="hint">You're sending ${s.sent_packets.toLocaleString()} packets so far. Loss and ping are for audio arriving from each person.</p>`
      : `<p class="hint">Nobody else is in the call yet.</p>`;
    const order = s.peers.map((p) => p.peer);
    for (const c of box.querySelectorAll("canvas")) drawSeries(c, order.map((p) => history.get(p) || []), c.dataset.metric, SAMPLES);
  };
  const timer = setInterval(tick, 500);
  tick();
}

function drawSeries(canvas, series, metric, samples) {
  const dpr = window.devicePixelRatio || 1;
  const w = canvas.clientWidth || 300;
  const h = canvas.clientHeight || 90;
  canvas.width = w * dpr;
  canvas.height = h * dpr;
  const ctx = canvas.getContext("2d");
  ctx.scale(dpr, dpr);
  const values = series.flat().map((x) => x[metric]).filter((v) => v != null);
  const max = metric === "rtt" ? Math.max(100, ...values) * 1.15 : Math.max(5, ...values) * 1.15;
  ctx.strokeStyle = "rgba(255,255,255,0.08)";
  ctx.fillStyle = "#949ba4";
  ctx.font = "10px sans-serif";
  for (const f of [0.5, 1]) {
    const y = h - f * (h - 4);
    ctx.beginPath();
    ctx.moveTo(0, y);
    ctx.lineTo(w, y);
    ctx.stroke();
    ctx.fillText(String(Math.round(max * f)), 2, y + 11);
  }
  series.forEach((pts, i) => {
    ctx.strokeStyle = STAT_COLORS[i % STAT_COLORS.length];
    ctx.lineWidth = 1.5;
    ctx.beginPath();
    let pen = false;
    pts.forEach((p, j) => {
      const v = p[metric];
      if (v == null) return (pen = false);
      const x = w - ((pts.length - 1 - j) / (samples - 1)) * w;
      const y = h - (v / max) * (h - 4);
      if (pen) ctx.lineTo(x, y);
      else ctx.moveTo(x, y);
      pen = true;
    });
    ctx.stroke();
  });
}

/** Cameras, with names. Browsers hide the names until camera access is granted once. */
async function listCameras() {
  if (!navigator.mediaDevices?.enumerateDevices) return [];
  const list = async () => (await navigator.mediaDevices.enumerateDevices()).filter((d) => d.kind === "videoinput");
  let cams = await list().catch(() => []);
  if (cams.length && cams.some((c) => !c.label)) {
    try {
      const s = await navigator.mediaDevices.getUserMedia({ video: true, audio: false });
      s.getTracks().forEach((t) => t.stop());
      cams = await list();
    } catch {
      /* no permission: unnamed cameras are still selectable */
    }
  }
  return cams;
}

export async function joinVoice(serverId, channel) {
  if (state.inVoice?.channel === channel) return;
  if (state.inVoice) await leaveVoice();
  if (ANDROID) {
    // Native audio can only open the mic once Android has granted the permission,
    // and asking through the webview is what shows the system prompt.
    try {
      const s = await navigator.mediaDevices.getUserMedia({ audio: true });
      s.getTracks().forEach((t) => t.stop());
    } catch {
      toast("Microphone permission was denied, so you'll join listen-only.", { error: true });
    }
  }
  const warnings = await call("voice_join", { server: serverId, channel });
  state.inVoice = { server: serverId, channel };
  for (const w of warnings) toast(w, { error: true, timeout: 8000 });
  await invoke("set_mute", { muted: state.muted, deafened: state.deafened }).catch(() => {});
  renderNav();
  renderCall();
}

export async function leaveVoice() {
  await stopShare();
  await stopCamera();
  for (const p of [...state.watching]) stopWatching(p);
  for (const id of [...viewers.keys()]) dropViewer(id);
  await call("voice_leave");
  state.inVoice = null;
  state.focus = null;
  if (state.callChannel) state.callChannel = null;
  render();
}

export async function setMute(muted, deafened) {
  state.muted = muted;
  state.deafened = deafened;
  await invoke("set_mute", { muted, deafened }).catch(() => {});
  renderUser();
  renderCall();
}

// Polling also tells peers when we start or stop talking (see voice_levels).
setInterval(() => {
  if (state.inVoice) invoke("voice_levels").catch(() => {});
}, 150);

// ---------- video: camera & screen share ----------
//
// Frame payload (sent to Rust, then encrypted per recipient):
//   flags u8 (bit 0 = keyframe) | codec u8 (1 = H.264, 2 = VP8) | width u16 | height u16 | timestamp µs u64 | data
// Incoming frames arrive as: kind u8 (2 = screen, 3 = camera) | peer id (32) | payload.

const SCREEN = { kind: 2, h264: "avc1.640033", bitrate: 6_000_000, framerate: 60, keyEvery: 240, push: "push_video" }; // High@5.1: 1440p60
const CAMERA = { kind: 3, h264: "avc1.42E01F", bitrate: 900_000, framerate: 30, keyEvery: 60, push: "push_camera" }; // Baseline: 720p30
const HEADER = 14;
const H264 = 1;
const VP8 = 2;

/** H.264 where this device can encode it, else VP8 (every WebCodecs build has it). */
async function pickCodec(cfg, width, height) {
  const base = { width, height, bitrate: cfg.bitrate, framerate: cfg.framerate };
  try {
    if ((await VideoEncoder.isConfigSupported({ ...base, codec: cfg.h264, avc: { format: "annexb" } })).supported) return { id: H264, codec: cfg.h264 };
  } catch {
    /* fall through to VP8 */
  }
  return { id: VP8, codec: "vp8" };
}

/** Decoder codec string for a frame's codec byte. */
function decoderCodec(id, kind) {
  return id === VP8 ? "vp8" : kind === 2 ? SCREEN.h264 : CAMERA.h264;
}

function canEncode() {
  return !!(window.VideoEncoder && window.VideoFrame);
}

/** Encodes `track` with WebCodecs and pushes frames to Rust. Returns controls. */
async function startEncoder(track, cfg) {
  const settings = track.getSettings();
  const width = (settings.width || 1280) & ~1;
  const height = (settings.height || 720) & ~1;
  const chosen = await pickCodec(cfg, width, height);
  let forceKey = true;
  let frames = 0;
  let stopped = false;
  // Nobody to send to (no watchers, or alone in the call): stop encoding, and
  // just probe with a keyframe now and then. A new viewer asks for a keyframe
  // straight away, which wakes us up.
  let idle = false;
  let lastProbe = 0;
  const encoder = new VideoEncoder({
    output: (chunk) => {
      const buf = new Uint8Array(HEADER + chunk.byteLength);
      const view = new DataView(buf.buffer);
      buf[0] = chunk.type === "key" ? 1 : 0;
      buf[1] = chosen.id;
      view.setUint16(2, width);
      view.setUint16(4, height);
      view.setBigUint64(6, BigInt(Math.max(0, Math.round(chunk.timestamp))));
      chunk.copyTo(buf.subarray(HEADER));
      invoke(cfg.push, buf)
        .then((sent) => (idle = sent === 0))
        .catch(() => {});
    },
    error: (e) => toast(`Video encoder error: ${e.message}`, { error: true }),
  });
  encoder.configure({
    codec: chosen.codec,
    width,
    height,
    bitrate: cfg.bitrate,
    framerate: cfg.framerate,
    latencyMode: "realtime",
    // Not "prefer-hardware": Chromium then fails outright on machines without a
    // hardware encoder instead of falling back. The default still uses hardware when present.
    ...(chosen.id === H264 ? { avc: { format: "annexb" } } : {}),
  });
  const encode = (frame) => {
    // Drop frames rather than queue them when the encoder falls behind.
    if (stopped || encoder.state !== "configured" || encoder.encodeQueueSize > 2) {
      frame.close();
      return;
    }
    if (idle && !forceKey) {
      if (performance.now() - lastProbe < 2000) {
        frame.close();
        return;
      }
      lastProbe = performance.now();
      forceKey = true;
    }
    const keyFrame = forceKey || frames % cfg.keyEvery === 0;
    forceKey = false;
    frames++;
    encoder.encode(frame, { keyFrame });
    frame.close();
  };
  if (window.MediaStreamTrackProcessor) {
    const reader = new MediaStreamTrackProcessor({ track }).readable.getReader();
    (async () => {
      while (!stopped) {
        const { value, done } = await reader.read();
        if (done) break;
        encode(value);
      }
    })();
  } else {
    const video = document.createElement("video");
    video.srcObject = new MediaStream([track]);
    video.muted = true;
    video.play().then(() => {
      const tick = () => {
        if (stopped) return;
        encode(new VideoFrame(video, { timestamp: performance.now() * 1000 }));
        video.requestVideoFrameCallback(tick);
      };
      video.requestVideoFrameCallback(tick);
    });
  }
  return {
    requestKeyframe: () => (forceKey = true),
    stop: () => {
      stopped = true;
      track.stop();
      if (encoder.state !== "closed") encoder.close();
    },
  };
}

function previewVideo(stream, mirror) {
  const v = document.createElement("video");
  v.srcObject = stream;
  v.muted = true;
  v.autoplay = true;
  v.playsInline = true;
  if (mirror) v.classList.add("mirror");
  v.play().catch(() => {});
  return v;
}

export async function startShare() {
  if (!navigator.mediaDevices?.getDisplayMedia || !canEncode()) {
    toast("Screen sharing isn't supported by this device's webview yet. It works in the Windows app.", { error: true, timeout: 8000 });
    return;
  }
  let stream;
  try {
    stream = await navigator.mediaDevices.getDisplayMedia({
      video: { frameRate: { ideal: 60 }, width: { ideal: 1920 }, height: { ideal: 1080 } },
      audio: false,
    });
  } catch {
    return; // picker cancelled
  }
  const track = stream.getVideoTracks()[0];
  const enc = await startEncoder(track, SCREEN);
  track.onended = () => stopShare();
  state.sharing = { ...enc, preview: previewVideo(stream, false) };
  await call("set_live", { live: true });
  renderVoicePanel();
  renderCall();
}

export async function stopShare() {
  if (!state.sharing) return;
  state.sharing.stop();
  state.sharing = null;
  if (state.focus === `2|${state.me.id}`) state.focus = null;
  await invoke("set_live", { live: false }).catch(() => {});
  renderVoicePanel();
  renderCall();
}

async function startCamera() {
  if (!navigator.mediaDevices?.getUserMedia || !canEncode()) {
    toast("Cameras aren't supported by this device's webview yet.", { error: true, timeout: 8000 });
    return;
  }
  let stream;
  try {
    stream = await openCamera();
  } catch (e) {
    toast(`Couldn't open your camera: ${e.message || e.name}`, { error: true });
    return;
  }
  state.camera = await cameraFrom(stream);
  await call("set_camera", { on: true });
  renderCall();
}

/** Opens the chosen camera, falling back to the default if it's gone. */
export async function openCamera() {
  const video = { width: { ideal: 1280 }, height: { ideal: 720 }, frameRate: { ideal: 30 } };
  const want = state.settings.camera;
  if (!want) return navigator.mediaDevices.getUserMedia({ video, audio: false });
  try {
    return await navigator.mediaDevices.getUserMedia({ video: { ...video, deviceId: { exact: want } }, audio: false });
  } catch (e) {
    if (!["OverconstrainedError", "NotFoundError"].includes(e.name)) throw e;
    toast("Your chosen camera isn't connected; using the default.", { error: true });
    return navigator.mediaDevices.getUserMedia({ video, audio: false });
  }
}

async function cameraFrom(stream) {
  const track = stream.getVideoTracks()[0];
  const enc = await startEncoder(track, CAMERA);
  track.onended = () => stopCamera();
  return { ...enc, preview: previewVideo(stream, true) };
}

/** Moves a live camera to the newly chosen device without turning it off for others. */
async function switchCamera() {
  const old = state.camera;
  if (!old) return;
  let stream;
  try {
    stream = await openCamera();
  } catch (e) {
    toast(`Couldn't open that camera: ${e.message || e.name}`, { error: true });
    return;
  }
  const next = await cameraFrom(stream);
  // The camera was turned off (or switched again) while this one was opening.
  if (state.camera !== old) {
    next.stop();
    return;
  }
  old.stop();
  state.camera = next;
  renderCall();
}

async function stopCamera() {
  if (!state.camera) return;
  state.camera.stop();
  state.camera = null;
  if (state.focus === `3|${state.me.id}`) state.focus = null;
  await invoke("set_camera", { on: false }).catch(() => {});
  renderCall();
}

// Receiving: one decoder + canvas per `${kind}|${peer}`.
export const viewers = new Map();

function hex(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function makeViewer(id) {
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d");
  const kind = +id.split("|")[0];
  const v = { canvas, waitingForKey: true, frames: 0, decoder: null, codecId: 0 };
  // (Re)creates the decoder for whatever codec the sender is using.
  v.configure = (codecId) => {
    if (v.decoder && v.decoder.state !== "closed") v.decoder.close();
    v.codecId = codecId;
    v.decoder = new VideoDecoder({
      output: (frame) => {
        // Decode everything (later frames depend on it) but only draw what can be seen.
        if (v.frames > 0 && (document.hidden || !canvas.isConnected)) {
          frame.close();
          return;
        }
        if (canvas.width !== frame.displayWidth) canvas.width = frame.displayWidth;
        if (canvas.height !== frame.displayHeight) canvas.height = frame.displayHeight;
        ctx.drawImage(frame, 0, 0);
        frame.close();
        if (v.frames++ === 0) renderCall(); // first picture: swap the avatar for video
      },
      error: () => {
        // Start over from the next keyframe.
        v.waitingForKey = true;
        v.codecId = 0;
      },
    });
    v.decoder.configure({ codec: decoderCodec(codecId, kind), optimizeForLatency: true });
  };
  viewers.set(id, v);
  return v;
}

export function dropViewer(id) {
  const v = viewers.get(id);
  if (v?.decoder && v.decoder.state !== "closed") v.decoder.close();
  viewers.delete(id);
}

export function onVideo(buf) {
  if (!window.VideoDecoder) return;
  const data = new Uint8Array(buf);
  const kind = data[0];
  const peer = hex(data.subarray(1, 33));
  const id = `${kind}|${peer}`;
  let v = viewers.get(id);
  if (!v) {
    if (kind !== 3) return; // screen shares only decode while you're watching
    v = makeViewer(id);
  }
  const frame = data.subarray(33);
  const view = new DataView(frame.buffer, frame.byteOffset);
  const keyFrame = (frame[0] & 1) === 1;
  if (v.waitingForKey && !keyFrame) return;
  if (keyFrame && frame[1] !== v.codecId) v.configure(frame[1]);
  v.waitingForKey = false;
  try {
    v.decoder.decode(new EncodedVideoChunk({ type: keyFrame ? "key" : "delta", timestamp: Number(view.getBigUint64(6)), data: frame.subarray(HEADER) }));
  } catch {
    v.waitingForKey = true;
  }
}

export function startWatching(peer) {
  if (!window.VideoDecoder) {
    toast("Watching streams isn't supported by this device's webview yet.", { error: true });
    return;
  }
  makeViewer(`2|${peer}`);
  state.watching.add(peer);
  state.focus = `2|${peer}`;
  invoke("watch", { peer, on: true }).catch((e) => toast(String(e), { error: true }));
  renderCall();
  renderChannels();
}

export function stopWatching(peer) {
  dropViewer(`2|${peer}`);
  state.watching.delete(peer);
  if (state.focus === `2|${peer}`) state.focus = null;
  invoke("watch", { peer, on: false }).catch(() => {});
  renderCall();
  renderChannels();
}

// ---------- call view ----------

/** The call shown on screen right now: { server, channel } or null. */
function shownCall() {
  if (state.view !== "home" && state.callChannel) return { server: state.view, channel: state.callChannel };
  if (state.view === "home" && state.homeTab !== "friends") {
    const s = server(state.homeTab);
    const vc = s?.channels.find((c) => c.voice);
    if (vc) return { server: s.id, channel: vc.id };
  }
  return null;
}

export function renderCall() {
  const el = $("#call");
  const shown = shownCall();
  if (!el || !shown) return;
  const room = state.voice[shown.server]?.[shown.channel] || roomOf({});
  const inThis = state.inVoice?.server === shown.server && state.inVoice?.channel === shown.channel;
  const peers = [...room.peers];
  if (inThis && !peers.includes(state.me.id)) peers.push(state.me.id);
  const compact = el.classList.contains("compact");
  if (compact && !peers.length) {
    el.hidden = true;
    el.innerHTML = "";
    return;
  }
  el.hidden = false;

  // Tiles: one per person, plus one per screen share.
  const tiles = [];
  for (const p of peers) {
    const me = p === state.me.id;
    const muted = me ? inThis && (state.muted || state.deafened) : room.muted.includes(p);
    const deafened = me ? inThis && state.deafened : room.deafened.includes(p);
    let media = null;
    if (me && state.camera) media = state.camera.preview;
    else if (!me && room.camera.includes(p) && viewers.get(`3|${p}`)?.frames) media = viewers.get(`3|${p}`).canvas;
    tiles.push({ id: `3|${p}`, peer: p, media, muted, deafened, kind: "person" });
  }
  for (const p of room.live) {
    const me = p === state.me.id;
    let media = null;
    if (me && state.sharing) media = state.sharing.preview;
    else if (!me && state.watching.has(p)) media = viewers.get(`2|${p}`)?.canvas || null;
    tiles.push({ id: `2|${p}`, peer: p, media, kind: "screen", watchable: !me && inThis && !state.watching.has(p) });
  }
  if (state.focus && !tiles.some((t) => t.id === state.focus)) state.focus = null;

  const tileEl = (t) => {
    const div = document.createElement("div");
    div.className = `tile ${t.kind}${state.focus === t.id ? " focused" : ""}${t.kind === "person" && state.speaking.has(key(shown.server, t.peer)) ? " speaking" : ""}`;
    div.dataset.tile = t.id;
    if (t.kind === "person") {
      div.dataset.peer = t.peer;
      div.dataset.server = shown.server;
    }
    div.style.setProperty("--tint", `hsl(${hue(t.peer)} 35% 22%)`);
    if (t.media) {
      div.classList.add("has-video");
      div.append(t.media);
    } else if (t.kind === "person") {
      div.insertAdjacentHTML("beforeend", `<div class="tile-avatar">${avatar(t.peer, "xl")}</div>`);
    } else {
      div.insertAdjacentHTML(
        "beforeend",
        `<div class="tile-screen"><span class="live-badge">LIVE</span>${t.watchable ? `<button class="btn primary" data-watch-tile="${t.peer}">Watch stream</button>` : `<span class="hint">${inThis ? "Starting…" : "Join the call to watch"}</span>`}</div>`,
      );
    }
    const icons = (t.muted ? `<span title="Muted">🔇</span>` : "") + (t.deafened ? `<span title="Deafened">🔕</span>` : "");
    const label = t.kind === "screen" ? `${esc(who(t.peer))}'s screen` : esc(who(t.peer));
    div.insertAdjacentHTML("beforeend", `<div class="tile-label">${icons}${label}</div>`);
    if (t.kind === "screen" && state.watching.has(t.peer)) {
      div.insertAdjacentHTML("beforeend", `<button class="tile-close" data-unwatch="${t.peer}" title="Stop watching">✕</button>`);
    }
    div.onclick = (e) => {
      if (e.target.closest("button")) return;
      state.focus = state.focus === t.id ? null : t.id;
      renderCall();
    };
    div.ondblclick = () => (document.fullscreenElement ? document.exitFullscreen() : div.requestFullscreen?.());
    return div;
  };

  el.innerHTML = "";
  const stageEl = document.createElement("div");
  stageEl.className = state.focus ? "tiles spotlight" : `tiles n${Math.min(tiles.length, 9)}`;
  if (state.focus) {
    const main = tiles.find((t) => t.id === state.focus);
    const big = tileEl(main);
    big.classList.add("big");
    stageEl.append(big);
    const strip = document.createElement("div");
    strip.className = "strip";
    for (const t of tiles.filter((t) => t.id !== state.focus)) strip.append(tileEl(t));
    stageEl.append(strip);
  } else {
    for (const t of tiles) stageEl.append(tileEl(t));
  }
  el.append(stageEl);

  const bar = document.createElement("div");
  bar.className = "call-controls";
  if (inThis) {
    bar.innerHTML = `
      <button class="ctl${state.muted ? " off" : ""}" data-ctl="mute" title="${state.muted ? "Unmute" : "Mute"}">${state.muted ? "🔇" : "🎙"}</button>
      <button class="ctl${state.deafened ? " off" : ""}" data-ctl="deafen" title="${state.deafened ? "Undeafen" : "Deafen"}">${state.deafened ? "🔕" : "🎧"}</button>
      <button class="ctl${state.camera ? " on" : ""}" data-ctl="camera" title="${state.camera ? "Turn off camera" : "Turn on camera"}">📷</button>
      ${navigator.mediaDevices?.getDisplayMedia ? `<button class="ctl${state.sharing ? " on" : ""}" data-ctl="share" title="${state.sharing ? "Stop sharing" : "Share your screen"}">🖥</button>` : ""}
      <button class="ctl" data-ctl="devices" title="Microphone, speakers & camera">⚙</button>
      <button class="ctl leave" data-ctl="leave" title="Leave call">📞</button>`;
  } else {
    bar.innerHTML = `<button class="btn join-call" data-ctl="join">Join call</button>`;
  }
  el.append(bar);

  el.querySelectorAll("[data-watch-tile]").forEach((b) => (b.onclick = () => startWatching(b.dataset.watchTile)));
  el.querySelectorAll("[data-unwatch]").forEach((b) => (b.onclick = () => stopWatching(b.dataset.unwatch)));
  bar.querySelectorAll("[data-ctl]").forEach(
    (b) =>
      (b.onclick = () => {
        const c = b.dataset.ctl;
        if (c === "mute") setMute(!state.muted, state.deafened);
        if (c === "deafen") setMute(state.muted, !state.deafened);
        if (c === "camera") (state.camera ? stopCamera() : startCamera());
        if (c === "share") (state.sharing ? stopShare() : startShare());
        if (c === "devices") devicesModal();
        if (c === "leave") leaveVoice();
        if (c === "join") joinVoice(shown.server, shown.channel);
      }),
  );
}
