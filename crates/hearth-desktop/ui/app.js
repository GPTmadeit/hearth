// Hearth desktop UI. Talks to the Rust engine through Tauri commands and the
// "hearth" event stream; screen share is captured and coded here with WebCodecs.

const { invoke, Channel } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel, root = document) => root.querySelector(sel);

const state = {
  me: null,
  servers: [],
  server: null, // current server id
  channel: null, // current text channel id
  names: {},
  online: new Set(),
  voice: {}, // server -> channel -> { peers: [], live: [] }
  invites: [], // { server, from, name }
  messages: {}, // `${server}|${channel}` -> [message]
  unread: new Set(), // `${server}|${channel}`
  inVoice: null, // { server, channel }
  muted: false,
  deafened: false,
  speaking: new Set(),
  sharing: null, // { stop() }
  watching: new Set(), // peer ids
};

// ---------- helpers ----------

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

function who(peer) {
  if (peer === state.me?.id) return state.me.name;
  return state.names[peer] || peer.slice(0, 8);
}

function hue(id) {
  let h = 0;
  for (const c of id) h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}

function avatar(peer, size = "md", extra = "") {
  const name = who(peer);
  return `<div class="avatar ${size} ${extra}" style="background:hsl(${hue(peer)} 55% 45%)">${esc(name[0]?.toUpperCase() || "?")}</div>`;
}

function initials(name) {
  return (name || "?")
    .split(/\s+/)
    .map((w) => w[0])
    .join("")
    .slice(0, 3)
    .toUpperCase();
}

function server() {
  return state.servers.find((s) => s.id === state.server);
}

function isOwner(s = server()) {
  return s && s.owner === state.me.id;
}

function key(server, channel) {
  return `${server}|${channel}`;
}

function toast(text, { error = false, action, actionLabel, timeout = 5000 } = {}) {
  const el = document.createElement("div");
  el.className = "toast" + (error ? " error" : "");
  el.innerHTML = `<span>${esc(text)}</span>`;
  if (action) {
    const b = document.createElement("button");
    b.className = "btn primary";
    b.textContent = actionLabel;
    b.onclick = () => {
      el.remove();
      action();
    };
    el.append(b);
  }
  $("#toasts").append(el);
  setTimeout(() => el.remove(), timeout);
}

async function call(cmd, args) {
  try {
    return await invoke(cmd, args);
  } catch (e) {
    toast(String(e), { error: true });
    throw e;
  }
}

async function copy(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast("Copied to clipboard");
  } catch {
    toast("Couldn't copy. Select the text instead.", { error: true });
  }
}

// ---------- modal ----------

function modal({ title, body = "", fields = [], submit = "OK", onSubmit, extra = "" }) {
  const root = $("#modal-root");
  root.innerHTML = `
    <div class="backdrop">
      <form class="modal">
        <h2>${esc(title)}</h2>
        ${body ? `<p>${body}</p>` : ""}
        ${extra}
        ${fields
          .map((f) =>
            f.type === "checkbox"
              ? `<label class="check"><input type="checkbox" name="${f.name}"> ${esc(f.label)}</label>`
              : `<label for="f-${f.name}">${esc(f.label)}</label><input id="f-${f.name}" name="${f.name}" value="${esc(f.value || "")}" placeholder="${esc(f.placeholder || "")}" autocomplete="off">`,
          )
          .join("")}
        <div class="actions">
          <button type="button" class="btn" data-cancel>Cancel</button>
          ${onSubmit ? `<button class="btn primary">${esc(submit)}</button>` : ""}
        </div>
      </form>
    </div>`;
  const form = $("form", root);
  const close = () => (root.innerHTML = "");
  $("[data-cancel]", root).onclick = close;
  $(".backdrop", root).onclick = (e) => e.target.classList.contains("backdrop") && close();
  root.querySelectorAll("[data-copy]").forEach((b) => (b.onclick = () => copy(b.dataset.copy)));
  form.onsubmit = async (e) => {
    e.preventDefault();
    const values = {};
    for (const f of fields) {
      const input = form.elements[f.name];
      values[f.name] = f.type === "checkbox" ? input.checked : input.value.trim();
    }
    try {
      await onSubmit(values);
      close();
    } catch {
      /* call() already showed the error */
    }
  };
  form.querySelector("input")?.focus();
}

function idBox(id) {
  return `<div class="copybox"><span>${esc(id)}</span><button type="button" class="btn" data-copy="${esc(id)}">Copy</button></div>`;
}

// ---------- data ----------

async function refreshServers() {
  state.servers = await invoke("servers");
  if (state.server && !server()) state.server = null;
  if (!state.server && state.servers.length) state.server = state.servers[0].id;
  const s = server();
  if (s && (!state.channel || !s.channels.some((c) => c.id === state.channel))) {
    state.channel = s.channels.find((c) => !c.voice)?.id || null;
    if (state.channel) await loadHistory();
  }
  render();
}

async function loadHistory() {
  if (!state.server || !state.channel) return;
  state.messages[key(state.server, state.channel)] = await invoke("history", {
    server: state.server,
    channel: state.channel,
    limit: 500,
  });
}

async function selectServer(id) {
  state.server = id;
  state.channel = server()?.channels.find((c) => !c.voice)?.id || null;
  await loadHistory();
  state.unread.delete(key(state.server, state.channel));
  render();
}

async function selectChannel(id) {
  state.channel = id;
  state.unread.delete(key(state.server, id));
  await loadHistory();
  render();
  $("#composer")?.focus();
}

// ---------- events from the engine ----------

async function onUpdate(u) {
  switch (u.type) {
    case "peer_online":
      state.names[u.peer] = u.name;
      state.online.add(u.peer);
      renderMembers();
      break;
    case "peer_offline":
      state.online.delete(u.peer);
      renderMembers();
      break;
    case "message": {
      const k = key(u.server, u.message.channel);
      (state.messages[k] ||= []).push(u.message);
      if (u.server === state.server && u.message.channel === state.channel) {
        renderMessages();
      } else if (u.message.author !== state.me.id) {
        state.unread.add(k);
        renderRail();
        renderChannels();
      }
      break;
    }
    case "server_changed":
      await refreshServers();
      break;
    case "removed":
      toast(`You were removed from ${state.servers.find((s) => s.id === u.server)?.name || "a server"}`, { error: true });
      break;
    case "invited":
      if (!state.invites.some((i) => i.server === u.server)) {
        state.invites.push(u);
        renderRail();
        toast(`${who(u.from)} invited you to ${u.name}`, {
          action: () => acceptInvite(u),
          actionLabel: "Join",
          timeout: 15000,
        });
      }
      break;
    case "dm":
      toast(`${who(u.peer)}: ${u.body}`);
      break;
    case "voice":
      ((state.voice[u.server] ||= {})[u.channel] = { peers: u.peers, live: u.live });
      for (const p of [...state.watching]) {
        if (!Object.values(state.voice[u.server]).some((r) => r.live.includes(p))) stopWatching(p);
      }
      renderChannels();
      break;
    case "keyframe_requested":
      state.sharing?.requestKeyframe();
      break;
  }
}

async function acceptInvite(inv) {
  state.invites = state.invites.filter((i) => i.server !== inv.server);
  await call("join", { server: inv.server, via: inv.from });
  toast(`Joining ${inv.name}…`);
  state.server = inv.server;
  renderRail();
}

// ---------- rendering ----------

function render() {
  renderRail();
  renderSidebar();
  renderMain();
  renderMembers();
}

function renderRail() {
  const rail = $("#rail");
  rail.innerHTML = "";
  for (const s of state.servers) {
    const b = document.createElement("button");
    b.className = "rail-btn" + (s.id === state.server ? " active" : "");
    b.title = s.name || "Syncing…";
    b.textContent = initials(s.name);
    if ([...state.unread].some((k) => k.startsWith(s.id + "|")) && s.id !== state.server) {
      b.insertAdjacentHTML("beforeend", `<span class="pip"></span>`);
    }
    b.onclick = () => selectServer(s.id);
    rail.append(b);
  }
  if (state.servers.length) rail.insertAdjacentHTML("beforeend", `<div class="rail-sep"></div>`);
  const add = document.createElement("button");
  add.className = "rail-btn add";
  add.title = "Create a server";
  add.textContent = "+";
  add.onclick = createServerModal;
  rail.append(add);
  if (state.invites.length) {
    const inv = document.createElement("button");
    inv.className = "rail-btn";
    inv.title = "Invites";
    inv.innerHTML = `✉<span class="badge">${state.invites.length}</span>`;
    inv.onclick = invitesModal;
    rail.append(inv);
  }
}

function renderSidebar() {
  const s = server();
  $("#server-header").innerHTML = s
    ? `<span class="name">${esc(s.name || "Syncing…")}</span>${isOwner(s) ? `<button class="icon-btn" id="server-menu" title="Server settings">⚙</button>` : ""}`
    : `<span class="name">Hearth</span>`;
  $("#server-menu")?.addEventListener("click", serverMenu);
  renderChannels();
  renderVoicePanel();
  renderUser();
}

function renderChannels() {
  const s = server();
  const el = $("#channels");
  if (!s) {
    el.innerHTML = "";
    return;
  }
  const text = s.channels.filter((c) => !c.voice);
  const voice = s.channels.filter((c) => c.voice);
  const owner = isOwner(s);
  const label = (name, isVoice) =>
    `<div class="section-label"><span>${name}</span>${owner ? `<button class="icon-btn" data-new="${isVoice}" title="Create channel">+</button>` : ""}</div>`;

  let html = label("Text channels", false);
  for (const c of text) {
    const cls = ["channel", c.id === state.channel && "active", state.unread.has(key(s.id, c.id)) && "unread"];
    html += `<button class="${cls.filter(Boolean).join(" ")}" data-text="${esc(c.id)}"><span class="glyph">#</span>${esc(c.name)}</button>`;
  }
  html += label("Voice channels", true);
  for (const c of voice) {
    const room = state.voice[s.id]?.[c.id] || { peers: [], live: [] };
    const here = state.inVoice?.channel === c.id;
    html += `<button class="channel${here ? " active" : ""}" data-voice="${esc(c.id)}"><span class="glyph">🔊</span>${esc(c.name)}</button>`;
    if (room.peers.length) {
      html += `<div class="voice-users">`;
      for (const p of room.peers) {
        const live = room.live.includes(p);
        const canWatch = live && p !== state.me.id && state.inVoice?.channel === c.id;
        const badge = canWatch
          ? `<button class="live-badge" data-watch="${p}" title="Watch stream">${state.watching.has(p) ? "WATCHING" : "LIVE"}</button>`
          : live
            ? `<span class="live-badge">LIVE</span>`
            : "";
        html += `<div class="voice-user">${avatar(p, "sm", state.speaking.has(p) ? "speaking" : "")}${esc(who(p))}${badge}</div>`;
      }
      html += `</div>`;
    }
  }
  el.innerHTML = html;
  el.querySelectorAll("[data-text]").forEach((b) => (b.onclick = () => selectChannel(b.dataset.text)));
  el.querySelectorAll("[data-voice]").forEach((b) => (b.onclick = () => joinVoice(b.dataset.voice)));
  el.querySelectorAll("[data-new]").forEach((b) => (b.onclick = () => createChannelModal(b.dataset.new === "true")));
  el.querySelectorAll("[data-watch]").forEach(
    (b) =>
      (b.onclick = () => (state.watching.has(b.dataset.watch) ? stopWatching(b.dataset.watch) : startWatching(b.dataset.watch))),
  );
}

function renderVoicePanel() {
  const panel = $("#voice-panel");
  if (!state.inVoice) {
    panel.hidden = true;
    return;
  }
  const s = state.servers.find((x) => x.id === state.inVoice.server);
  const c = s?.channels.find((x) => x.id === state.inVoice.channel);
  panel.hidden = false;
  panel.innerHTML = `
    <div class="state">Voice connected</div>
    <div class="where">${esc(c?.name || "")} / ${esc(s?.name || "")}</div>
    <div class="row">
      <button class="pill${state.sharing ? " on" : ""}" id="share">${state.sharing ? "Stop sharing" : "Share screen"}</button>
      <button class="pill danger" id="hangup">Leave</button>
    </div>`;
  $("#share").onclick = () => (state.sharing ? stopShare() : startShare());
  $("#hangup").onclick = leaveVoice;
}

function renderUser() {
  if (!state.me) return;
  $("#user-panel").innerHTML = `
    ${avatar(state.me.id, "md")}
    <div class="who"><div>${esc(state.me.name)}</div><div class="id" title="Your peer ID">${esc(state.me.id.slice(0, 16))}…</div></div>
    <button class="toggle${state.muted ? " off" : ""}" id="mute" title="${state.muted ? "Unmute" : "Mute"}">${state.muted ? "🔇" : "🎙"}</button>
    <button class="toggle${state.deafened ? " off" : ""}" id="deafen" title="${state.deafened ? "Undeafen" : "Deafen"}">${state.deafened ? "🔕" : "🎧"}</button>
    <button class="toggle" id="settings" title="Settings">⚙</button>`;
  $("#mute").onclick = () => setMute(!state.muted, state.deafened);
  $("#deafen").onclick = () => setMute(state.muted, !state.deafened);
  $("#settings").onclick = settingsModal;
}

function renderMain() {
  const main = $("#main");
  const s = server();
  if (!s) {
    main.innerHTML = `
      <div class="empty">
        <h2>Welcome to Hearth</h2>
        <p>Private, peer-to-peer chat. No accounts and no central server: your ID is a key that only you hold.</p>
        <p><button class="btn primary" id="welcome-create">Create a server</button></p>
        <p>Or send a friend your ID so they can invite you:</p>
        ${idBox(state.me.id)}
      </div><div id="stage"></div>`;
    $("#welcome-create").onclick = createServerModal;
    main.querySelectorAll("[data-copy]").forEach((b) => (b.onclick = () => copy(b.dataset.copy)));
    return;
  }
  const c = s.channels.find((x) => x.id === state.channel);
  main.innerHTML = `
    <div class="main-header"><span class="glyph">#</span>${esc(c?.name || "")}<span class="e2e" title="Messages are encrypted to server members; nodes and relays only see ciphertext">🔒 end-to-end encrypted</span></div>
    <div id="stage"></div>
    <div id="messages"></div>
    <div class="composer"><textarea id="composer" rows="1" placeholder="Message #${esc(c?.name || "")}"></textarea></div>`;
  const box = $("#composer");
  box.onkeydown = async (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      const body = box.value.trim();
      if (!body || !state.channel) return;
      box.value = "";
      await call("send", { server: state.server, channel: state.channel, body });
    }
  };
  box.oninput = () => {
    box.style.height = "auto";
    box.style.height = box.scrollHeight + "px";
  };
  renderStage();
  renderMessages();
}

function renderMessages() {
  const el = $("#messages");
  if (!el) return;
  const list = state.messages[key(state.server, state.channel)] || [];
  if (!list.length) {
    el.innerHTML = `<div class="empty"><h2>No messages yet</h2><p>Say hello. Only members of this server can read it.</p></div>`;
    return;
  }
  let html = "";
  let prev = null;
  for (const m of list) {
    const t = new Date(m.sent_at_ms);
    const grouped = prev && prev.author === m.author && m.sent_at_ms - prev.sent_at_ms < 5 * 60_000;
    if (!grouped) {
      if (prev) html += `</div></div>`;
      html += `<div class="msg-group">${avatar(m.author, "lg")}<div><div class="msg-meta"><span class="author">${esc(who(m.author))}</span><time>${t.toLocaleString([], { dateStyle: "short", timeStyle: "short" })}</time></div>`;
    }
    html += `<div class="msg-body">${esc(m.body)}</div>`;
    prev = m;
  }
  html += `</div></div>`;
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
  el.innerHTML = html;
  if (atBottom || !el.dataset.init) el.scrollTop = el.scrollHeight;
  el.dataset.init = "1";
}

async function renderMembers() {
  const s = server();
  const el = $("#members");
  if (!s) {
    el.innerHTML = "";
    return;
  }
  const on = (p) => p === state.me.id || state.online.has(p);
  const sorted = [...s.members].sort((a, b) => on(b) - on(a) || who(a).localeCompare(who(b)));
  const owner = isOwner(s);
  let html = `<div class="section-label"><span>Members — ${s.members.length}</span>${owner ? `<button class="icon-btn" id="invite-btn" title="Invite">+</button>` : ""}</div>`;
  for (const p of sorted) {
    const tag = p === s.owner ? `<div class="tag">owner</div>` : "";
    const kick = owner && p !== state.me.id ? `<button class="icon-btn kick" data-kick="${p}" title="Remove from server">✕</button>` : "";
    html += `<div class="member${on(p) ? " on" : ""}">${avatar(p, "md").replace("</div>", `<span class="status${on(p) ? " on" : ""}"></span></div>`)}<div>${esc(who(p))}${tag}</div>${kick}</div>`;
  }
  if (s.hosts.length) {
    html += `<div class="section-label"><span>Storage nodes</span></div>`;
    for (const h of s.hosts) {
      html += `<div class="member${state.online.has(h) ? " on" : ""}"><div class="avatar md" style="background:#4e5058">⛁</div><div>${esc(h.slice(0, 10))}<div class="tag">blind node</div></div></div>`;
    }
  }
  el.innerHTML = html;
  $("#invite-btn")?.addEventListener("click", inviteModal);
  el.querySelectorAll("[data-kick]").forEach(
    (b) =>
      (b.onclick = () =>
        modal({
          title: `Remove ${who(b.dataset.kick)}?`,
          body: "They'll lose access to new messages. The server key is rotated so they can't read anything sent after this.",
          submit: "Remove",
          onSubmit: async () => {
            await call("kick", { server: s.id, peer: b.dataset.kick });
          },
        })),
  );
}

// ---------- modals ----------

function createServerModal() {
  modal({
    title: "Create a server",
    body: "Your server lives on members' devices (and any storage nodes you add), never on a company's servers.",
    fields: [{ name: "name", label: "Server name", placeholder: `${state.me.name}'s server` }],
    submit: "Create",
    onSubmit: async ({ name }) => {
      const id = await call("create_server", { name: name || `${state.me.name}'s server` });
      await refreshServers();
      await selectServer(id);
    },
  });
}

function createChannelModal(voice) {
  modal({
    title: voice ? "Create voice channel" : "Create text channel",
    fields: [{ name: "name", label: "Channel name", placeholder: voice ? "Hangout" : "new-channel" }],
    submit: "Create",
    onSubmit: async ({ name }) => {
      if (!name) throw new Error("name required");
      const id = await call("create_channel", { server: state.server, name, voice });
      await refreshServers();
      if (!voice) await selectChannel(id);
    },
  });
}

function inviteModal() {
  modal({
    title: `Invite to ${server()?.name}`,
    body: "Paste your friend's peer ID. They'll get an invite as soon as you're both online.",
    fields: [{ name: "peer", label: "Peer ID", placeholder: "64 hex characters" }],
    submit: "Invite",
    onSubmit: async ({ peer }) => {
      await call("invite", { server: state.server, peer });
      toast("Invite sent");
    },
  });
}

function serverMenu() {
  const s = server();
  modal({
    title: s.name || "Server",
    extra: `
      <label>Server ID</label>${idBox(s.id)}
      <label>Key epoch</label><div class="copybox"><span>${s.epoch}: rotates whenever someone is removed</span></div>`,
    fields: [{ name: "node", label: "Add a storage node (optional)", placeholder: "Node peer ID: stores encrypted history for offline delivery" }],
    submit: "Add node",
    onSubmit: async ({ node }) => {
      if (!node) return;
      await call("add_host", { server: s.id, peer: node });
      toast("Asked the node to host this server");
    },
  });
}

function invitesModal() {
  const root = $("#modal-root");
  modal({
    title: "Invites",
    extra: state.invites
      .map(
        (i, n) =>
          `<div class="member on"><div>${esc(i.name)}<div class="tag">from ${esc(who(i.from))}</div></div><button type="button" class="btn primary kick" style="visibility:visible" data-accept="${n}">Join</button></div>`,
      )
      .join(""),
  });
  root.querySelectorAll("[data-accept]").forEach(
    (b) =>
      (b.onclick = () => {
        root.innerHTML = "";
        acceptInvite(state.invites[+b.dataset.accept]);
      }),
  );
}

function settingsModal() {
  modal({
    title: "Settings",
    extra: `<label>Your peer ID</label>${idBox(state.me.id)}<p style="margin-top:8px">Share this so people can invite you. Your private key never leaves this device.</p>`,
    fields: [
      { name: "name", label: "Display name", value: state.me.name },
      { name: "dial", label: "Connect to a peer (optional)", placeholder: "Peer ID" },
    ],
    submit: "Save",
    onSubmit: async ({ name, dial }) => {
      if (name && name !== state.me.name) {
        await call("set_name", { name });
        state.me.name = name;
        toast("Name saved. Peers see it the next time you connect.");
      }
      if (dial) await call("dial", { peer: dial });
      renderUser();
    },
  });
}

// ---------- voice ----------

async function joinVoice(channel) {
  if (state.inVoice?.channel === channel) return;
  if (state.sharing) stopShare();
  const warnings = await call("voice_join", { server: state.server, channel });
  state.inVoice = { server: state.server, channel };
  for (const w of warnings) toast(w, { error: true, timeout: 8000 });
  await invoke("set_mute", { muted: state.muted, deafened: state.deafened });
  renderSidebar();
}

async function leaveVoice() {
  if (state.sharing) stopShare();
  for (const p of [...state.watching]) stopWatching(p);
  await call("voice_leave");
  state.inVoice = null;
  state.speaking.clear();
  renderSidebar();
}

async function setMute(muted, deafened) {
  state.muted = muted;
  state.deafened = deafened;
  await invoke("set_mute", { muted, deafened });
  renderUser();
}

setInterval(async () => {
  if (!state.inVoice) return;
  const levels = await invoke("voice_levels");
  if (!levels) return;
  const next = new Set(levels.speaking);
  if (levels.transmitting) next.add(state.me.id);
  const changed = next.size !== state.speaking.size || [...next].some((p) => !state.speaking.has(p));
  state.speaking = next;
  if (changed) renderChannels();
}, 200);

// ---------- screen share ----------
//
// Frame payload (sent to Rust, then encrypted per viewer):
//   flags u8 (bit 0 = keyframe) | codec u8 (1 = H.264) | width u16 | height u16 | timestamp µs u64 | data

const CODEC = "avc1.640033"; // H.264 High @ L5.1: 1440p60
const HEADER = 14;

function canShare() {
  return !!(navigator.mediaDevices?.getDisplayMedia && window.VideoEncoder && window.VideoFrame);
}

async function startShare() {
  if (!canShare()) {
    toast("Screen sharing needs WebCodecs, which this platform's webview doesn't support yet. It works in the Windows app.", {
      error: true,
      timeout: 8000,
    });
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
  const { width = 1920, height = 1080 } = track.getSettings();
  let forceKey = true;
  let frames = 0;
  let stopped = false;

  const encoder = new VideoEncoder({
    output: (chunk) => {
      const buf = new Uint8Array(HEADER + chunk.byteLength);
      const view = new DataView(buf.buffer);
      buf[0] = chunk.type === "key" ? 1 : 0;
      buf[1] = 1;
      view.setUint16(2, width);
      view.setUint16(4, height);
      view.setBigUint64(6, BigInt(Math.max(0, Math.round(chunk.timestamp))));
      chunk.copyTo(buf.subarray(HEADER));
      invoke("push_video", buf).catch(() => {});
    },
    error: (e) => {
      toast(`Encoder error: ${e.message}`, { error: true });
      stopShare();
    },
  });
  encoder.configure({
    codec: CODEC,
    width: width & ~1,
    height: height & ~1,
    bitrate: 6_000_000,
    framerate: 60,
    latencyMode: "realtime",
    hardwareAcceleration: "prefer-hardware",
    avc: { format: "annexb" },
  });

  const encode = (frame) => {
    // Drop frames rather than queue them when the encoder falls behind.
    if (encoder.encodeQueueSize > 2) {
      frame.close();
      return;
    }
    const keyFrame = forceKey || frames % 240 === 0;
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
    video.srcObject = stream;
    video.muted = true;
    await video.play();
    const tick = () => {
      if (stopped) return;
      encode(new VideoFrame(video, { timestamp: performance.now() * 1000 }));
      video.requestVideoFrameCallback(tick);
    };
    video.requestVideoFrameCallback(tick);
  }

  track.onended = () => stopShare();
  state.sharing = {
    requestKeyframe: () => (forceKey = true),
    stop: () => {
      stopped = true;
      track.stop();
      if (encoder.state !== "closed") encoder.close();
    },
  };
  await call("set_live", { live: true });
  renderVoicePanel();
}

async function stopShare() {
  if (!state.sharing) return;
  state.sharing.stop();
  state.sharing = null;
  await invoke("set_live", { live: false }).catch(() => {});
  renderVoicePanel();
}

// Viewing: one decoder + canvas per streamer.
const viewers = new Map(); // peer -> { decoder, canvas, waitingForKey }

function hex(bytes) {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

function onVideo(buf) {
  const data = new Uint8Array(buf);
  const peer = hex(data.subarray(0, 32));
  const v = viewers.get(peer);
  if (!v) return;
  const frame = data.subarray(32);
  const view = new DataView(frame.buffer, frame.byteOffset);
  const key = (frame[0] & 1) === 1;
  if (v.waitingForKey && !key) return;
  v.waitingForKey = false;
  try {
    v.decoder.decode(
      new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp: Number(view.getBigUint64(6)), data: frame.subarray(HEADER) }),
    );
  } catch {
    v.waitingForKey = true;
  }
}

function startWatching(peer) {
  if (!window.VideoDecoder) {
    toast("Watching streams needs WebCodecs, which this platform's webview doesn't support yet.", { error: true });
    return;
  }
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d");
  const v = {
    canvas,
    waitingForKey: true,
    decoder: new VideoDecoder({
      output: (frame) => {
        if (canvas.width !== frame.displayWidth) canvas.width = frame.displayWidth;
        if (canvas.height !== frame.displayHeight) canvas.height = frame.displayHeight;
        ctx.drawImage(frame, 0, 0);
        frame.close();
      },
      error: () => {
        // Recover by asking for a fresh keyframe.
        v.waitingForKey = true;
        v.decoder = null;
        stopWatching(peer);
        startWatching(peer);
      },
    }),
  };
  v.decoder.configure({ codec: CODEC, hardwareAcceleration: "prefer-hardware", optimizeForLatency: true });
  viewers.set(peer, v);
  state.watching.add(peer);
  invoke("watch", { peer, on: true }).catch((e) => toast(String(e), { error: true }));
  renderStage();
  renderChannels();
}

function stopWatching(peer) {
  const v = viewers.get(peer);
  if (v?.decoder && v.decoder.state !== "closed") v.decoder.close();
  viewers.delete(peer);
  state.watching.delete(peer);
  invoke("watch", { peer, on: false }).catch(() => {});
  renderStage();
  renderChannels();
}

function renderStage() {
  const stage = $("#stage");
  if (!stage) return;
  stage.innerHTML = "";
  for (const [peer, v] of viewers) {
    const tile = document.createElement("div");
    tile.className = "tile";
    tile.append(v.canvas);
    tile.insertAdjacentHTML("beforeend", `<span class="label">${esc(who(peer))}</span><button class="close" title="Stop watching">✕</button>`);
    $(".close", tile).onclick = () => stopWatching(peer);
    tile.ondblclick = () => (document.fullscreenElement ? document.exitFullscreen() : tile.requestFullscreen());
    stage.append(tile);
  }
}

// ---------- boot ----------

async function boot() {
  state.me = await invoke("me");
  state.names = await invoke("names");
  for (const p of await invoke("online")) state.online.add(p);
  const ch = new Channel();
  ch.onmessage = onVideo;
  await invoke("video_channel", { channel: ch });
  await listen("hearth", (e) => onUpdate(e.payload));
  await refreshServers();
}

boot().catch((e) => {
  document.body.innerHTML = `<div class="empty"><h2>Hearth couldn't start</h2><p>${esc(e)}</p></div>`;
});
