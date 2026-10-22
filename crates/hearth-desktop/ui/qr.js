// QR codes and shareable links: showing, scanning, using.

import { $, call, modal, refreshFriends, toast } from "./app.js";
import { openCamera } from "./call.js";

const { invoke } = window.__TAURI__.core;

// ---------- QR codes & links ----------

/** Renders a QR code for the link `text` (a promise) into `el`, with the link to copy. */
export async function showQr(el, text) {
  try {
    const link = await text;
    el.innerHTML = `<div class="qr">${await invoke("qr_svg", { text: link })}</div>`;
    el.dataset.link = link;
  } catch (e) {
    el.textContent = `Couldn't make a QR code: ${e}`;
  }
}

/** Acts on a scanned or pasted link: friend request or server join. */
export async function useLink(link) {
  const kind = await call("open_link", { link });
  if (kind === "friend") {
    toast("Friend request sent");
    await refreshFriends();
  } else {
    toast("Asked to join. You'll be let in as soon as the owner's device is online.", { timeout: 8000 });
  }
  return kind;
}

/** Scans a QR code with the camera (or takes a pasted link). */
export function scanModal() {
  const root = modal({
    title: "Scan a QR code",
    body: "Point your camera at someone's friend code or a server invite code.",
    extra: `<div class="scanner"><video playsinline muted></video><div class="scan-frame"></div><p class="scan-status hint">Starting camera…</p></div>`,
    fields: [{ name: "link", label: "Or paste a link", placeholder: "hearth://… or a peer ID" }],
    submit: "Use link",
    onSubmit: async ({ link }) => {
      if (!link) throw new Error("Paste a link, or scan a code");
      await useLink(link);
    },
  });
  const video = $(".scanner video", root);
  const status = $(".scan-status", root);
  const canvas = document.createElement("canvas");
  let stream = null;
  let busy = false;
  let done = false;
  const stop = () => {
    clearInterval(timer);
    stream?.getTracks().forEach((t) => t.stop());
  };
  const timer = setInterval(async () => {
    if (!root.contains(video) || !video.isConnected) return stop();
    if (busy || done || !video.videoWidth) return;
    busy = true;
    try {
      const text = await invoke("scan_qr", grayFrame(video, canvas));
      if (text && !done) {
        done = true;
        status.textContent = "Found a code…";
        try {
          await useLink(text);
          root.innerHTML = "";
        } catch (e) {
          done = false;
          status.textContent = String(e?.message || e);
        }
      }
    } catch {
      /* try the next frame */
    } finally {
      busy = false;
    }
  }, 250);
  (async () => {
    try {
      stream = await openCamera();
      if (!video.isConnected) return stop();
      video.srcObject = stream;
      await video.play();
      status.textContent = "Looking for a code…";
    } catch (e) {
      status.textContent = `No camera available (${e.message || e.name}). You can paste a link instead.`;
    }
  })();
}

/** The middle of a video frame as `width | height | greyscale pixels`, for the QR reader. */
function grayFrame(video, canvas) {
  const scale = Math.min(1, 640 / video.videoWidth);
  const w = Math.round(video.videoWidth * scale);
  const h = Math.round(video.videoHeight * scale);
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  ctx.drawImage(video, 0, 0, w, h);
  const rgba = ctx.getImageData(0, 0, w, h).data;
  const out = new Uint8Array(8 + w * h);
  const dv = new DataView(out.buffer);
  dv.setUint32(0, w, true);
  dv.setUint32(4, h, true);
  for (let i = 0, j = 8; i < rgba.length; i += 4, j++) out[j] = (rgba[i] * 77 + rgba[i + 1] * 150 + rgba[i + 2] * 29) >> 8;
  return out;
}
