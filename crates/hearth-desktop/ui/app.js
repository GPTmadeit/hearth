// Hearth desktop UI. Talks to the Rust engine through Tauri commands and the
// "hearth" event stream; screen share is captured and coded here with WebCodecs.

import { CATEGORIES, expandShortcodes, search as searchEmoji } from "./emoji.js";

const { invoke, Channel } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (sel, root = document) => root.querySelector(sel);
const $$ = (sel, root = document) => [...root.querySelectorAll(sel)];

const MAX_FILE = 100 * 1024 * 1024;
const ANDROID = /Android/i.test(navigator.userAgent);
const AUTOLOAD_MEDIA = 25 * 1024 * 1024;

const state = {
  me: null,
  servers: [],
  view: "home", // "home" or a server id
  homeTab: "friends", // "friends" or a DM server id
  channel: null, // current text channel in server view
  friendsTab: "online",
  names: {},
  online: new Set(),
  friends: [],
  avatars: {}, // peer -> blob ref
  avatarUrls: {}, // blob hash -> object URL
  voice: {}, // server -> channel -> { peers, live }
  speaking: new Set(), // `${server}|${peer}`
  typing: {}, // `${server}|${channel}` -> { peer: expiresAt }
  invites: [],
  messages: {}, // `${server}|${channel}` -> [message]
  unread: {}, // key -> count
  mentions: {}, // key -> count
  inVoice: null,
  muted: false,
  deafened: false,
  sharing: null, // { stop, requestKeyframe, preview }
  camera: null, // { stop, requestKeyframe, preview }
  watching: new Set(),
  callChannel: null, // server voice channel shown full-screen, if any
  focus: null, // tile id in spotlight
  reply: null, // message being replied to
  editing: null, // message being edited
  pending: [], // attachments: { id, name, size, mime, preview, ready, dto, error }
  settings: loadSettings(),
};

// ---------- helpers ----------

function loadSettings() {
  try {
    return { dnd: false, notify: {}, recentEmoji: [], ...JSON.parse(localStorage.getItem("hearth.settings") || "{}") };
  } catch {
    return { dnd: false, notify: {}, recentEmoji: [] };
  }
}

function saveSettings() {
  try {
    localStorage.setItem("hearth.settings", JSON.stringify(state.settings));
  } catch {
    /* storage unavailable: settings last for this session */
  }
}

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
  const ref = state.avatars[peer];
  const url = ref && state.avatarUrls[ref.hash];
  const inner = url ? `<img src="${url}" alt="">` : esc(who(peer)[0]?.toUpperCase() || "?");
  return `<div class="avatar ${size} ${extra}" style="background:hsl(${hue(peer)} 55% 45%)" data-peer="${peer}">${inner}</div>`;
}

function initials(name) {
  return (name || "?")
    .split(/\s+/)
    .map((w) => w[0])
    .join("")
    .slice(0, 3)
    .toUpperCase();
}

function fmtSize(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function key(server, channel) {
  return `${server}|${channel}`;
}

function server(id = state.view) {
  return state.servers.find((s) => s.id === id);
}

function isOwner(s) {
  return s && s.owner === state.me.id;
}

function textChannel(s) {
  return s?.channels.find((c) => !c.voice);
}

/** A voice room from an update or snapshot, with every list present. */
function roomOf(r) {
  return { peers: r.peers || [], live: r.live || [], camera: r.camera || [], muted: r.muted || [], deafened: r.deafened || [] };
}

function friendOf(peer) {
  return state.friends.find((f) => f.peer === peer);
}

function dmFor(peer) {
  return state.servers.find((s) => s.dm === peer);
}

/** The conversation on screen: { server, channel } or null. */
function current() {
  if (state.view === "home") {
    const s = server(state.homeTab);
    return s ? { server: s.id, channel: textChannel(s)?.id } : null;
  }
  return state.channel ? { server: state.view, channel: state.channel } : null;
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

async function call(cmd, args, options) {
  try {
    return await invoke(cmd, args, options);
  } catch (e) {
    toast(String(e), { error: true });
    throw Object.assign(new Error(String(e)), { shown: true });
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

function modal({ title, body = "", fields = [], submit = "OK", onSubmit, extra = "", danger = false }) {
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
              ? `<label class="check"><input type="checkbox" name="${f.name}" ${f.value ? "checked" : ""}> ${esc(f.label)}</label>`
              : f.type === "select"
                ? `<label for="f-${f.name}">${esc(f.label)}</label><select id="f-${f.name}" name="${f.name}">${f.options.map(([v, l]) => `<option value="${esc(v)}" ${v === f.value ? "selected" : ""}>${esc(l)}</option>`).join("")}</select>`
                : `<label for="f-${f.name}">${esc(f.label)}</label><input id="f-${f.name}" name="${f.name}" value="${esc(f.value || "")}" placeholder="${esc(f.placeholder || "")}" autocomplete="off">`,
          )
          .join("")}
        <div class="actions">
          <button type="button" class="btn" data-cancel>Cancel</button>
          ${onSubmit ? `<button class="btn ${danger ? "danger" : "primary"}">${esc(submit)}</button>` : ""}
        </div>
      </form>
    </div>`;
  const form = $("form", root);
  const close = () => (root.innerHTML = "");
  $("[data-cancel]", root).onclick = close;
  // A block body on purpose: an onclick property that returns false cancels the
  // default action, which would stop the form's submit button from submitting.
  $(".backdrop", root).onclick = (e) => {
    if (e.target === e.currentTarget) close();
  };
  $$("[data-copy]", root).forEach((b) => (b.onclick = () => copy(b.dataset.copy)));
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
    } catch (err) {
      if (!err?.shown) toast(String(err?.message || err), { error: true });
    }
  };
  form.querySelector("input,select")?.focus();
  return root;
}

/** Drawer and member-list toggles, shown only on narrow screens. */
function navButtons() {
  return `<button class="icon-btn big nav-toggle" data-toggle="nav-open" title="Menu">☰</button>`;
}

function membersButton() {
  return `<button class="icon-btn big nav-toggle members-toggle" data-toggle="members-open" title="Members">👥</button>`;
}

function closeDrawers() {
  document.body.classList.remove("nav-open", "members-open");
}

document.addEventListener("click", (e) => {
  if (e.target.id === "scrim") return closeDrawers();
  const t = e.target.closest("[data-toggle]");
  if (t) {
    const cls = t.dataset.toggle;
    const on = !document.body.classList.contains(cls);
    closeDrawers();
    document.body.classList.toggle(cls, on);
  }
});

function idBox(id) {
  return `<div class="copybox"><span>${esc(id)}</span><button type="button" class="btn" data-copy="${esc(id)}">Copy</button></div>`;
}

// ---------- data loading ----------

async function refreshServers() {
  state.servers = await invoke("servers");
  if (state.view !== "home" && !server()) state.view = "home";
  if (state.view === "home" && state.homeTab !== "friends" && !server(state.homeTab)) state.homeTab = "friends";
  const s = state.view !== "home" && server();
  if (s && (!state.channel || !s.channels.some((c) => c.id === state.channel && !c.voice))) {
    state.channel = textChannel(s)?.id || null;
  }
  await loadHistory();
  render();
}

async function refreshFriends() {
  state.friends = await invoke("friends");
}

async function loadHistory() {
  const c = current();
  if (!c?.channel) return;
  state.messages[key(c.server, c.channel)] = await invoke("history", { server: c.server, channel: c.channel, limit: 500 });
}

async function loadAvatars() {
  state.avatars = await invoke("avatars");
  for (const [peer, ref] of Object.entries(state.avatars)) loadAvatar(peer, ref);
}

async function loadAvatar(peer, ref) {
  if (state.avatarUrls[ref.hash]) return;
  try {
    const bytes = await invoke("fetch_blob", { blob: ref, prefer: [peer] });
    state.avatarUrls[ref.hash] = URL.createObjectURL(new Blob([bytes]));
    // Swap the initial for the picture wherever this person is shown.
    $$(`.avatar[data-peer="${peer}"]`).forEach((el) => (el.innerHTML = `<img src="${state.avatarUrls[ref.hash]}" alt="">`));
  } catch {
    /* they're offline; try again when their profile next arrives */
  }
}

// ---------- navigation ----------

async function openHome(tab = "friends") {
  closeDrawers();
  state.view = "home";
  state.homeTab = tab;
  state.reply = state.editing = null;
  if (tab !== "friends") {
    const c = current();
    if (c) delete state.unread[key(c.server, c.channel)];
    await loadHistory();
  }
  render();
  $("#composer")?.focus();
}

async function openServer(id) {
  closeDrawers();
  state.callChannel = state.inVoice?.server === id ? state.callChannel : null;
  state.view = id;
  state.channel = textChannel(server(id))?.id || null;
  state.reply = state.editing = null;
  if (state.channel) delete state.unread[key(id, state.channel)];
  await loadHistory();
  render();
}

function openCall(channel) {
  closeDrawers();
  state.callChannel = channel;
  state.focus = null;
  renderMain();
  renderChannels();
}

async function openChannel(id) {
  closeDrawers();
  state.callChannel = null;
  state.channel = id;
  state.reply = state.editing = null;
  delete state.unread[key(state.view, id)];
  delete state.mentions[key(state.view, id)];
  await loadHistory();
  render();
  $("#composer")?.focus();
}

async function openDm(peer) {
  const s = dmFor(peer);
  if (!s) {
    toast(`Your conversation with ${who(peer)} is still being set up. Try again in a moment.`);
    return;
  }
  await openHome(s.id);
}

// ---------- notifications ----------

let audioCtx;
function ping() {
  try {
    audioCtx ||= new AudioContext();
    const o = audioCtx.createOscillator();
    const g = audioCtx.createGain();
    o.frequency.value = 880;
    g.gain.setValueAtTime(0.08, audioCtx.currentTime);
    g.gain.exponentialRampToValueAtTime(0.0001, audioCtx.currentTime + 0.25);
    o.connect(g).connect(audioCtx.destination);
    o.start();
    o.stop(audioCtx.currentTime + 0.25);
  } catch {
    /* no audio output */
  }
}

function mentionsMe(body) {
  const name = state.me.name.toLowerCase();
  const b = body.toLowerCase();
  return b.includes(`@${name}`) || b.includes("@everyone");
}

function notify(title, body) {
  if (state.settings.dnd) return;
  ping();
  invoke("notify", { title, body }).catch(() => {});
}

function updateTitle() {
  const n = Object.values(state.unread).reduce((a, b) => a + b, 0) + state.friends.filter((f) => f.state === "incoming").length;
  document.title = n ? `(${n}) Hearth` : "Hearth";
}

function onMessageNotice(serverId, m) {
  if (m.author === state.me.id) return;
  const s = server(serverId);
  const k = key(serverId, m.channel);
  const c = current();
  const viewing = c && key(c.server, c.channel) === k && document.hasFocus();
  if (!viewing) {
    state.unread[k] = (state.unread[k] || 0) + 1;
    if (mentionsMe(m.body)) state.mentions[k] = (state.mentions[k] || 0) + 1;
  }
  if (viewing) return;
  const text = m.body || (m.attachments.length ? `sent ${m.attachments.length === 1 ? "a file" : `${m.attachments.length} files`}` : "");
  if (s?.dm) {
    notify(who(m.author), text);
    return;
  }
  const level = state.settings.notify[serverId] || "all";
  if (level === "none" || (level === "mentions" && !mentionsMe(m.body))) return;
  const ch = s?.channels.find((x) => x.id === m.channel)?.name || "";
  notify(`${who(m.author)} in #${ch} (${s?.name || "server"})`, text);
}

// ---------- events from the engine ----------

async function onUpdate(u) {
  switch (u.type) {
    case "peer_online":
      state.names[u.peer] = u.name;
      state.online.add(u.peer);
      renderLight();
      break;
    case "peer_offline":
      state.online.delete(u.peer);
      for (const k of [...state.speaking]) if (k.endsWith("|" + u.peer)) state.speaking.delete(k);
      renderLight();
      break;
    case "message": {
      const k = key(u.server, u.message.channel);
      (state.messages[k] ||= []).push(u.message);
      clearTyping(u.server, u.message.channel, u.message.author);
      onMessageNotice(u.server, u.message);
      const c = current();
      if (c && key(c.server, c.channel) === k) renderMessages();
      renderNav();
      break;
    }
    case "message_updated": {
      const list = state.messages[key(u.server, u.message.channel)];
      const i = list?.findIndex((m) => m.id === u.message.id) ?? -1;
      if (i >= 0) list[i] = u.message;
      const c = current();
      if (c && c.server === u.server && c.channel === u.message.channel) renderMessages();
      break;
    }
    case "server_changed":
      await refreshServers();
      break;
    case "server_removed":
      await refreshServers();
      break;
    case "removed":
      toast(`You were removed from ${server(u.server)?.name || "a server"}`, { error: true });
      break;
    case "invited":
      if (!state.invites.some((i) => i.server === u.server)) {
        state.invites.push(u);
        renderNav();
        notify("Server invite", `${who(u.from)} invited you to ${u.name}`);
        toast(`${who(u.from)} invited you to ${u.name}`, { action: () => acceptInvite(u), actionLabel: "Join", timeout: 15000 });
      }
      break;
    case "friend":
      await refreshFriends();
      if (u.request) {
        notify("Friend request", `${who(u.peer)} wants to be friends`);
        toast(`${who(u.peer)} sent you a friend request`, { action: () => call("add_friend", { peer: u.peer }), actionLabel: "Accept", timeout: 15000 });
      }
      await refreshServers();
      break;
    case "profile": {
      state.names = await invoke("names");
      if (u.peer === state.me.id) state.me.name = state.names[u.peer] || state.me.name;
      state.avatars = await invoke("avatars");
      const ref = state.avatars[u.peer];
      if (ref) loadAvatar(u.peer, ref);
      render();
      break;
    }
    case "dm":
      notify(who(u.peer), u.body);
      toast(`${who(u.peer)}: ${u.body}`);
      break;
    case "voice":
      ((state.voice[u.server] ||= {})[u.channel] = roomOf(u));
      for (const id of [...viewers.keys()]) {
        const [kind, peer] = id.split("|");
        if (kind === "3" && !Object.values(state.voice[u.server]).some((r) => r.camera.includes(peer))) dropViewer(id);
      }
      for (const k of [...state.speaking]) {
        const [s, p] = k.split("|");
        if (s === u.server && !Object.values(state.voice[s]).some((r) => r.peers.includes(p))) state.speaking.delete(k);
      }
      for (const p of [...state.watching]) {
        if (!Object.values(state.voice[u.server]).some((r) => r.live.includes(p))) stopWatching(p);
      }
      renderLight();
      renderCall();
      break;
    case "speaking": {
      const k = key(u.server, u.peer);
      if (u.speaking) state.speaking.add(k);
      else state.speaking.delete(k);
      $$(`.avatar[data-peer="${u.peer}"]`).forEach((el) => {
        if (el.closest(`[data-speak-server="${u.server}"]`)) el.classList.toggle("speaking", u.speaking);
      });
      $$(`.tile[data-peer="${u.peer}"][data-server="${u.server}"]`).forEach((el) => el.classList.toggle("speaking", u.speaking));
      break;
    }
    case "typing": {
      const k = key(u.server, u.channel);
      (state.typing[k] ||= {})[u.peer] = Date.now() + 6000;
      renderTyping();
      break;
    }
    case "keyframe_requested":
      (u.camera ? state.camera : state.sharing)?.requestKeyframe();
      break;
  }
  updateTitle();
}

function clearTyping(serverId, channel, peer) {
  const t = state.typing[key(serverId, channel)];
  if (t) delete t[peer];
  renderTyping();
}

async function acceptInvite(inv) {
  state.invites = state.invites.filter((i) => i.server !== inv.server);
  await call("join", { server: inv.server, via: inv.from });
  toast(`Joining ${inv.name}…`);
  state.view = inv.server;
  renderNav();
}

// ---------- rendering ----------

function render() {
  renderNav();
  renderMain();
  renderSide();
  updateTitle();
}

/** Re-renders presence-driven pieces without touching the composer. */
function renderLight() {
  renderChannels();
  renderSide();
  if (state.view === "home" && state.homeTab === "friends") renderMain();
}

function renderNav() {
  renderRail();
  renderSidebar();
}

function unreadIn(serverId) {
  let unread = 0;
  let mentions = 0;
  for (const [k, n] of Object.entries(state.unread)) if (k.startsWith(serverId + "|")) unread += n;
  for (const [k, n] of Object.entries(state.mentions)) if (k.startsWith(serverId + "|")) mentions += n;
  return { unread, mentions };
}

function renderRail() {
  const rail = $("#rail");
  rail.innerHTML = "";
  const home = document.createElement("button");
  home.className = "rail-btn home" + (state.view === "home" ? " active" : "");
  home.title = "Friends & messages";
  home.innerHTML = `<svg viewBox="0 0 24 24" width="26" height="26" aria-hidden="true"><path fill="currentColor" d="M12 2c1.5 3 5 5.2 5 9.5A5 5 0 0 1 12 17a5 5 0 0 1-5-5.5C7 9 8.6 7.6 9.5 6 10 8 11 8.8 12 9c-.6-2.4-.4-4.6 0-7Zm-6 17h12v2H6z"/></svg>`;
  const dmUnread = state.servers.filter((s) => s.dm).reduce((n, s) => n + unreadIn(s.id).unread, 0);
  const requests = state.friends.filter((f) => f.state === "incoming").length;
  if (dmUnread + requests) home.insertAdjacentHTML("beforeend", `<span class="badge">${dmUnread + requests}</span>`);
  home.onclick = () => openHome("friends");
  rail.append(home);
  rail.insertAdjacentHTML("beforeend", `<div class="rail-sep"></div>`);

  for (const s of state.servers.filter((x) => !x.dm)) {
    const b = document.createElement("button");
    b.className = "rail-btn" + (s.id === state.view ? " active" : "");
    b.title = s.name || "Syncing…";
    b.textContent = initials(s.name);
    const { unread, mentions } = unreadIn(s.id);
    if (mentions) b.insertAdjacentHTML("beforeend", `<span class="badge">${mentions}</span>`);
    else if (unread && s.id !== state.view) b.insertAdjacentHTML("beforeend", `<span class="pip"></span>`);
    b.onclick = () => openServer(s.id);
    rail.append(b);
  }
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
  if (state.view === "home") {
    $("#server-header").innerHTML = `<span class="name">Direct messages</span>`;
  } else {
    const s = server();
    $("#server-header").innerHTML = s
      ? `<span class="name">${esc(s.name || "Syncing…")}</span><button class="icon-btn" id="server-menu" title="Server settings">⚙</button>`
      : `<span class="name">Hearth</span>`;
    $("#server-menu")?.addEventListener("click", serverMenu);
  }
  renderChannels();
  renderVoicePanel();
  renderUser();
}

function renderChannels() {
  const el = $("#channels");
  if (state.view === "home") return renderDmList(el);
  const s = server();
  if (!s) {
    el.innerHTML = "";
    return;
  }
  const owner = isOwner(s);
  const label = (name, isVoice) =>
    `<div class="section-label"><span>${name}</span>${owner ? `<button class="icon-btn" data-new="${isVoice}" title="Create channel">+</button>` : ""}</div>`;

  let html = label("Text channels", false);
  for (const c of s.channels.filter((c) => !c.voice)) {
    const k = key(s.id, c.id);
    const cls = ["channel", c.id === state.channel && "active", state.unread[k] && "unread"].filter(Boolean).join(" ");
    const badge = state.mentions[k] ? `<span class="count">${state.mentions[k]}</span>` : "";
    html += `<button class="${cls}" data-text="${esc(c.id)}"><span class="glyph">#</span><span class="cname">${esc(c.name)}</span>${badge}</button>`;
  }
  html += label("Voice channels", true);
  for (const c of s.channels.filter((c) => c.voice)) html += voiceChannelHtml(s, c);
  el.innerHTML = html;
  el.querySelectorAll("[data-text]").forEach((b) => (b.onclick = () => openChannel(b.dataset.text)));
  el.querySelectorAll("[data-new]").forEach((b) => (b.onclick = () => createChannelModal(b.dataset.new === "true")));
  wireVoiceList(el, s);
}

function voiceChannelHtml(s, c, label) {
  const room = state.voice[s.id]?.[c.id] || roomOf({});
  const here = state.inVoice?.channel === c.id;
  const showing = state.callChannel === c.id && state.view === s.id;
  let html = `<button class="channel${here ? " connected" : ""}${showing ? " active" : ""}" data-voice="${esc(c.id)}" data-voice-server="${s.id}"><span class="glyph">🔊</span><span class="cname">${esc(label || c.name)}</span></button>`;
  if (room.peers.length) {
    html += `<div class="voice-users" data-speak-server="${s.id}">`;
    for (const p of room.peers) {
      const live = room.live.includes(p);
      const canWatch = live && p !== state.me.id && here;
      const badge = canWatch
        ? `<button class="live-badge" data-watch="${p}" title="Watch stream">${state.watching.has(p) ? "WATCHING" : "LIVE"}</button>`
        : live
          ? `<span class="live-badge">LIVE</span>`
          : "";
      html += `<div class="voice-user">${avatar(p, "sm", state.speaking.has(key(s.id, p)) ? "speaking" : "")}<span>${esc(who(p))}</span>${badge}</div>`;
    }
    html += `</div>`;
  }
  return html;
}

function wireVoiceList(el, s) {
  el.querySelectorAll("[data-voice]").forEach(
    (b) =>
      (b.onclick = async () => {
        await joinVoice(b.dataset.voiceServer || s.id, b.dataset.voice);
        if (!server(b.dataset.voiceServer || s.id)?.dm) openCall(b.dataset.voice);
      }),
  );
  el.querySelectorAll("[data-watch]").forEach(
    (b) => (b.onclick = () => (state.watching.has(b.dataset.watch) ? stopWatching(b.dataset.watch) : startWatching(b.dataset.watch))),
  );
}

function lastActivity(s) {
  const c = textChannel(s);
  const list = c && state.messages[key(s.id, c.id)];
  return list?.length ? list[list.length - 1].sent_at_ms : 0;
}

function renderDmList(el) {
  const pending = state.friends.filter((f) => f.state === "incoming").length;
  let html = `<button class="channel${state.homeTab === "friends" ? " active" : ""}" id="friends-tab"><span class="glyph">👥</span><span class="cname">Friends</span>${pending ? `<span class="count">${pending}</span>` : ""}</button>`;
  html += `<div class="section-label"><span>Direct messages</span></div>`;
  const dms = state.servers.filter((s) => s.dm).sort((a, b) => lastActivity(b) - lastActivity(a));
  for (const s of dms) {
    const c = textChannel(s);
    const k = c && key(s.id, c.id);
    const unread = k && state.unread[k];
    const on = state.online.has(s.dm);
    const cls = ["channel dm", s.id === state.homeTab && "active", unread && "unread"].filter(Boolean).join(" ");
    const av = avatar(s.dm, "md").replace(/<\/div>$/, `<span class="status${on ? " on" : ""}"></span></div>`);
    html += `<button class="${cls}" data-dm="${s.id}">${av}<span class="cname">${esc(who(s.dm))}</span>${unread ? `<span class="count">${unread}</span>` : ""}</button>`;
  }
  if (!dms.length) html += `<p class="hint">Add a friend to start a private conversation.</p>`;
  el.innerHTML = html;
  $("#friends-tab").onclick = () => openHome("friends");
  el.querySelectorAll("[data-dm]").forEach((b) => (b.onclick = () => openHome(b.dataset.dm)));
}

function renderVoicePanel() {
  const panel = $("#voice-panel");
  if (!state.inVoice) {
    panel.hidden = true;
    return;
  }
  const s = server(state.inVoice.server);
  const c = s?.channels.find((x) => x.id === state.inVoice.channel);
  const where = s?.dm ? `Call with ${who(s.dm)}` : `${c?.name || ""} / ${s?.name || ""}`;
  panel.hidden = false;
  panel.innerHTML = `
    <button class="state" id="open-call" title="Show the call">Voice connected</button>
    <div class="where">${esc(where)}</div>
    <div class="row">
      ${navigator.mediaDevices?.getDisplayMedia ? `<button class="pill${state.sharing ? " on" : ""}" id="share">${state.sharing ? "Stop sharing" : "Share screen"}</button>` : ""}
      <button class="pill danger" id="hangup">Leave</button>
    </div>`;
  $("#share")?.addEventListener("click", () => (state.sharing ? stopShare() : startShare()));
  $("#hangup").onclick = leaveVoice;
  $("#open-call").onclick = () => {
    const sv = state.inVoice.server;
    if (server(sv)?.dm) openHome(sv);
    else {
      if (state.view !== sv) {
        state.view = sv;
        state.channel = textChannel(server(sv))?.id || null;
        renderNav();
      }
      openCall(state.inVoice.channel);
    }
  };
}

function renderUser() {
  if (!state.me) return;
  $("#user-panel").innerHTML = `
    <button class="who-btn" id="profile-btn" title="Profile & settings">
      ${avatar(state.me.id, "md")}
      <div class="who"><div>${esc(state.me.name)}</div><div class="id">${state.settings.dnd ? "Do not disturb" : esc(state.me.id.slice(0, 12)) + "…"}</div></div>
    </button>
    <button class="toggle${state.muted ? " off" : ""}" id="mute" title="${state.muted ? "Unmute" : "Mute"}">${state.muted ? "🔇" : "🎙"}</button>
    <button class="toggle${state.deafened ? " off" : ""}" id="deafen" title="${state.deafened ? "Undeafen" : "Deafen"}">${state.deafened ? "🔕" : "🎧"}</button>
    <button class="toggle" id="settings" title="Settings">⚙</button>`;
  $("#mute").onclick = () => setMute(!state.muted, state.deafened);
  $("#deafen").onclick = () => setMute(state.muted, !state.deafened);
  $("#settings").onclick = settingsModal;
  $("#profile-btn").onclick = settingsModal;
}

function renderMain() {
  const main = $("#main");
  if (state.view === "home" && state.homeTab === "friends") return renderFriendsPage(main);
  if (state.view !== "home" && state.callChannel) {
    const ch = server()?.channels.find((c) => c.id === state.callChannel);
    main.innerHTML = `<div class="main-header">${navButtons()}<span class="glyph">🔊</span>${esc(ch?.name || "Voice")}<span class="e2e" title="Audio and video are encrypted for members of this server">🔒 end-to-end encrypted</span></div><div class="call full" id="call"></div>`;
    renderCall();
    return;
  }
  const c = current();
  const s = c && server(c.server);
  if (!s) {
    main.innerHTML = `<div class="empty"><h2>Nothing here</h2></div>`;
    return;
  }
  const ch = s.channels.find((x) => x.id === c.channel);
  const title = s.dm ? `<span class="glyph">@</span>${esc(who(s.dm))}` : `<span class="glyph">#</span>${esc(ch?.name || "")}`;
  const voiceCh = s.dm && s.channels.find((x) => x.voice);
  const callBtn = voiceCh ? `<button class="icon-btn big" id="call-btn" title="Start a call">📞</button>` : "";
  const placeholder = s.dm ? `Message @${who(s.dm)}` : `Message #${ch?.name || ""}`;
  main.innerHTML = `
    <div class="main-header">${navButtons()}${title}${callBtn}<span class="e2e" title="Only members can read this; nodes and relays see ciphertext">🔒 end-to-end encrypted</span>${membersButton()}</div>
    ${s.dm ? `<div class="call compact" id="call"></div>` : ""}
    <div id="messages"></div>
    <div class="composer">
      <div id="composer-bars"></div>
      <div class="composer-row">
        <button class="icon-btn big" id="attach-btn" title="Attach files">＋</button>
        <textarea id="composer" rows="1" placeholder="${esc(placeholder)}"></textarea>
        <button class="icon-btn big" id="emoji-btn" title="Emoji">😀</button>
      </div>
      <div id="typing"></div>
      <input type="file" id="file-input" multiple hidden>
    </div>`;
  if (callBtn) $("#call-btn").onclick = () => joinVoice(s.id, voiceCh.id);
  wireComposer();
  renderCall();
  renderCall();
  renderMessages();
  renderComposerBars();
  renderTyping();
}

// ---------- friends page ----------

function renderFriendsPage(main) {
  const tabs = [
    ["online", "Online"],
    ["all", "All"],
    ["pending", "Pending"],
    ["blocked", "Blocked"],
  ];
  const pendingCount = state.friends.filter((f) => f.state === "incoming").length;
  let list = [];
  let empty = "";
  switch (state.friendsTab) {
    case "online":
      list = state.friends.filter((f) => f.state === "friends" && state.online.has(f.peer));
      empty = "No friends online right now.";
      break;
    case "all":
      list = state.friends.filter((f) => f.state === "friends");
      empty = "No friends yet. Add one with their peer ID.";
      break;
    case "pending":
      list = state.friends.filter((f) => f.state === "incoming" || f.state === "outgoing");
      empty = "No pending requests.";
      break;
    case "blocked":
      list = state.friends.filter((f) => f.state === "blocked");
      empty = "You haven't blocked anyone.";
      break;
  }
  list.sort((a, b) => who(a.peer).localeCompare(who(b.peer)));
  const rows = list
    .map((f) => {
      const on = state.online.has(f.peer);
      const status =
        f.state === "incoming" ? "Incoming friend request" : f.state === "outgoing" ? "Outgoing friend request" : f.state === "blocked" ? "Blocked" : on ? "Online" : "Offline";
      let actions = "";
      if (f.state === "friends") actions = `<button class="round" data-act="message" title="Message">💬</button><button class="round" data-act="remove" title="Remove friend">✕</button>`;
      if (f.state === "incoming") actions = `<button class="round ok" data-act="accept" title="Accept">✓</button><button class="round" data-act="remove" title="Decline">✕</button>`;
      if (f.state === "outgoing") actions = `<button class="round" data-act="remove" title="Cancel request">✕</button>`;
      if (f.state === "blocked") actions = `<button class="btn" data-act="unblock">Unblock</button>`;
      if (f.state !== "blocked") actions += `<button class="round" data-act="block" title="Block">⛔</button>`;
      const av = avatar(f.peer, "md").replace(/<\/div>$/, `<span class="status${on ? " on" : ""}"></span></div>`);
      return `<div class="friend-row" data-peer="${f.peer}">${av}<div class="friend-meta"><div class="fname">${esc(who(f.peer))}</div><div class="fstatus">${status}</div></div><div class="friend-actions">${actions}</div></div>`;
    })
    .join("");
  main.innerHTML = `
    <div class="main-header">${navButtons()}<span class="glyph">👥</span>Friends
      <nav class="tabs">${tabs.map(([t, l]) => `<button class="tab${state.friendsTab === t ? " active" : ""}" data-tab="${t}">${l}${t === "pending" && pendingCount ? ` <span class="count">${pendingCount}</span>` : ""}</button>`).join("")}
      <button class="tab add${state.friendsTab === "add" ? " active" : ""}" data-tab="add">Add friend</button></nav>
    </div>
    <div class="friends-page">
      ${
        state.friendsTab === "add"
          ? `<h3>Add a friend</h3><p class="hint">Paste their peer ID. They'll get your request as soon as they're online.</p>
             <form id="add-friend-form" class="add-friend"><input name="peer" placeholder="Peer ID (64 characters)" autocomplete="off"><button class="btn primary">Send request</button></form>
             <h3>Your ID</h3><p class="hint">Share this so friends can add you.</p>${idBox(state.me.id)}`
          : rows || `<div class="empty"><p>${empty}</p></div>`
      }
    </div>`;
  main.querySelectorAll("[data-tab]").forEach((b) => (b.onclick = () => ((state.friendsTab = b.dataset.tab), renderMain())));
  main.querySelectorAll("[data-copy]").forEach((b) => (b.onclick = () => copy(b.dataset.copy)));
  const form = $("#add-friend-form");
  if (form) {
    form.onsubmit = async (e) => {
      e.preventDefault();
      const peer = form.elements.peer.value.trim();
      if (!peer) return;
      try {
        await call("add_friend", { peer });
        form.reset();
        toast("Friend request sent");
        await refreshFriends();
        state.friendsTab = "pending";
        renderMain();
      } catch {
        /* toast already shown */
      }
    };
    form.elements.peer.focus();
  }
  main.querySelectorAll(".friend-row [data-act]").forEach((b) => {
    const peer = b.closest(".friend-row").dataset.peer;
    b.onclick = () => friendAction(peer, b.dataset.act);
  });
  renderCall();
}

async function friendAction(peer, act) {
  if (act === "message") return openDm(peer);
  if (act === "accept") await call("add_friend", { peer });
  if (act === "remove") await call("remove_friend", { peer });
  if (act === "unblock") await call("block", { peer, blocked: false });
  if (act === "block") {
    modal({
      title: `Block ${who(peer)}?`,
      body: "They won't be able to message you, call you, or send friend requests. Their posts in shared servers are hidden.",
      submit: "Block",
      danger: true,
      onSubmit: async () => {
        await call("block", { peer, blocked: true });
        await refreshFriends();
        render();
      },
    });
    return;
  }
  await refreshFriends();
  await refreshServers(); // accepting may have created a DM
}

// ---------- messages ----------

const URL_RE = /\bhttps?:\/\/[^\s<>"']+[^\s<>"'.,;:!?)\]]/g;

/** Minimal, safe markdown: escapes first, then adds formatting. */
function formatBody(text) {
  const blocks = [];
  let html = esc(text).replace(/```(?:[a-z]*\n)?([\s\S]*?)```/g, (_, code) => {
    blocks.push(`<pre><code>${code}</code></pre>`);
    return `\u0000${blocks.length - 1}\u0000`;
  });
  const inline = [];
  html = html.replace(/`([^`\n]+)`/g, (_, code) => {
    inline.push(`<code>${code}</code>`);
    return `\u0001${inline.length - 1}\u0001`;
  });
  html = html
    .replace(URL_RE, (u) => `<a href="#" data-url="${u}">${u}</a>`)
    .replace(/\*\*([^*\n]+)\*\*/g, "<strong>$1</strong>")
    .replace(/(^|[^*\w])\*([^*\n]+)\*(?!\w)/g, "$1<em>$2</em>")
    .replace(/(^|[^_\w])_([^_\n]+)_(?!\w)/g, "$1<em>$2</em>")
    .replace(/~~([^~\n]+)~~/g, "<s>$1</s>")
    .replace(/(^|\s)@([\w.-]{1,64})/g, (m, pre, name) => {
      const me = name.toLowerCase() === state.me.name.toLowerCase() || name === "everyone";
      return `${pre}<span class="mention${me ? " me" : ""}">@${name}</span>`;
    })
    .replace(/^&gt; (.*)$/gm, `<span class="quote">$1</span>`);
  html = html.replace(/\u0001(\d+)\u0001/g, (_, i) => inline[i]).replace(/\u0000(\d+)\u0000/g, (_, i) => blocks[i]);
  return html;
}

function isOnlyEmoji(text) {
  const t = text.trim();
  return t && t.length <= 24 && /^(\p{Extended_Pictographic}|\p{Emoji_Component}|‍|️|\s)+$/u.test(t) && !/^[\d#*\s]+$/.test(t);
}

function attachmentHtml(m, a, i) {
  const ref = `data-msg="${esc(m.id)}" data-att="${i}"`;
  if (a.mime.startsWith("image/")) {
    return `<div class="att att-image" ${ref}><div class="att-loading">Loading image…</div></div>`;
  }
  if (a.mime.startsWith("video/") || a.mime.startsWith("audio/")) {
    const kind = a.mime.startsWith("video/") ? "video" : "audio";
    return `<div class="att att-${kind}" ${ref}><button class="btn att-load">▶ Load ${kind} (${fmtSize(a.blob.size)})</button></div>`;
  }
  return `<div class="att att-file" ${ref}><span class="file-icon">📄</span><div class="file-meta"><div class="file-name">${esc(a.name)}</div><div class="file-size">${fmtSize(a.blob.size)}</div></div><button class="btn att-save">Download</button></div>`;
}

function messageHtml(s, m, grouped) {
  const owner = isOwner(s);
  const mine = m.author === state.me.id;
  const blocked = friendOf(m.author)?.state === "blocked";
  const t = new Date(m.sent_at_ms);
  let reply = "";
  if (m.reply_to) {
    const list = state.messages[key(s.id, m.channel)] || [];
    const target = list.find((x) => x.id === m.reply_to);
    reply = `<div class="reply-ref" data-jump="${esc(m.reply_to)}">↪ ${target ? `<b>${esc(who(target.author))}</b> ${esc((target.deleted ? "deleted message" : target.body.replace(/[*_~`>]/g, "") || "attachment").slice(0, 80))}` : "a message"}</div>`;
  }
  let body;
  if (m.deleted) body = `<div class="msg-body deleted">Message deleted</div>`;
  else if (blocked) body = `<div class="msg-body deleted">Blocked message</div>`;
  else {
    const big = isOnlyEmoji(m.body) && !m.attachments.length ? " jumbo" : "";
    body =
      (m.body ? `<div class="msg-body${big}${mentionsMe(m.body) ? " mentions-me" : ""}">${formatBody(m.body)}${m.edited ? ` <span class="edited">(edited)</span>` : ""}</div>` : "") +
      m.attachments.map((a, i) => attachmentHtml(m, a, i)).join("");
  }
  const reactions = Object.entries(m.reactions || {})
    .map(([e, who_]) => `<button class="reaction${who_.includes(state.me.id) ? " mine" : ""}" data-react="${esc(e)}" title="${esc(who_.map(who).join(", "))}">${esc(e)} <span>${who_.length}</span></button>`)
    .join("");
  const tools = m.deleted
    ? ""
    : `<div class="msg-tools">
        <button data-tool="react" title="Add reaction">😀</button>
        <button data-tool="reply" title="Reply">↩</button>
        ${mine ? `<button data-tool="edit" title="Edit">✏️</button>` : ""}
        ${mine || owner ? `<button data-tool="delete" title="Delete">🗑️</button>` : ""}
      </div>`;
  const head = grouped
    ? `<div class="msg-gutter"><time>${t.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}</time></div>`
    : `<div class="msg-gutter">${avatar(m.author, "lg")}</div>`;
  const meta = grouped
    ? ""
    : `<div class="msg-meta"><span class="author">${esc(who(m.author))}</span><time>${t.toLocaleString([], { dateStyle: "short", timeStyle: "short" })}</time></div>`;
  return `<div class="msg${grouped ? " grouped" : ""}${m.reply_to ? " has-reply" : ""}" data-id="${esc(m.id)}">${reply ? `<div class="reply-row">${reply}</div>` : ""}${head}<div class="msg-content">${meta}${body}${reactions ? `<div class="reactions">${reactions}<button class="reaction add" data-tool="react" title="Add reaction">＋</button></div>` : ""}</div>${tools}</div>`;
}

function renderMessages() {
  const el = $("#messages");
  const c = current();
  if (!el || !c) return;
  const s = server(c.server);
  const list = state.messages[key(c.server, c.channel)] || [];
  if (!list.length) {
    el.innerHTML = s?.dm
      ? `<div class="empty">${avatar(s.dm, "xl")}<h2>${esc(who(s.dm))}</h2><p>This is the start of your private conversation. Only the two of you can read it.</p></div>`
      : `<div class="empty"><h2>No messages yet</h2><p>Say hello. Only members of this server can read it.</p></div>`;
    return;
  }
  let html = "";
  let prev = null;
  for (const m of list) {
    const grouped = prev && prev.author === m.author && m.sent_at_ms - prev.sent_at_ms < 5 * 60_000 && !m.reply_to;
    html += messageHtml(s, m, grouped);
    prev = m;
  }
  const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
  el.innerHTML = html;
  if (atBottom || !el.dataset.init) el.scrollTop = el.scrollHeight;
  el.dataset.init = "1";
  wireMessages(el, s, list);
}

function wireMessages(el, s, list) {
  const byId = (id) => list.find((m) => m.id === id);
  el.querySelectorAll(".msg").forEach((row) => {
    const m = byId(row.dataset.id);
    row.addEventListener("click", (e) => {
      if (e.target.closest("button, a, img, video, audio")) return;
      const was = row.classList.contains("active");
      el.querySelectorAll(".msg.active").forEach((r) => r.classList.remove("active"));
      row.classList.toggle("active", !was);
    });
    row.querySelectorAll("[data-tool]").forEach((b) => {
      b.onclick = (e) => {
        e.stopPropagation();
        const tool = b.dataset.tool;
        if (tool === "react") openEmojiPicker(b, (emoji) => toggleReaction(s.id, m, emoji));
        if (tool === "reply") startReply(m);
        if (tool === "edit") startEdit(m);
        if (tool === "delete") confirmDelete(s.id, m);
      };
    });
    row.querySelectorAll("[data-react]").forEach((b) => (b.onclick = () => toggleReaction(s.id, m, b.dataset.react)));
    row.querySelector("[data-jump]")?.addEventListener("click", () => {
      const target = el.querySelector(`.msg[data-id="${CSS.escape(m.reply_to)}"]`);
      target?.scrollIntoView({ block: "center" });
      target?.classList.add("flash");
      setTimeout(() => target?.classList.remove("flash"), 1200);
    });
  });
  el.querySelectorAll(".att").forEach((box) => {
    const m = byId(box.dataset.msg);
    const a = m?.attachments[+box.dataset.att];
    if (!a) return;
    if (box.classList.contains("att-image")) loadMedia(box, m, a, "img");
    else if (box.classList.contains("att-video") || box.classList.contains("att-audio")) {
      const kind = box.classList.contains("att-video") ? "video" : "audio";
      if (a.blob.size <= AUTOLOAD_MEDIA && mediaCache.has(a.blob.hash)) loadMedia(box, m, a, kind);
      else box.querySelector(".att-load").onclick = () => loadMedia(box, m, a, kind);
    } else box.querySelector(".att-save").onclick = () => saveAttachment(m, a);
  });
  el.querySelectorAll("a[data-url]").forEach((link) => {
    link.onclick = (e) => {
      e.preventDefault();
      call("open_url", { url: link.dataset.url }).catch(() => {});
    };
  });
}

const mediaCache = new Map(); // blob hash -> object URL

async function blobUrl(m, a) {
  if (mediaCache.has(a.blob.hash)) return mediaCache.get(a.blob.hash);
  const bytes = await invoke("fetch_blob", { blob: a.blob, prefer: [m.author] });
  const url = URL.createObjectURL(new Blob([bytes], { type: a.mime }));
  mediaCache.set(a.blob.hash, url);
  return url;
}

async function loadMedia(box, m, a, kind) {
  box.innerHTML = `<div class="att-loading">Loading ${esc(a.name)}…</div>`;
  try {
    const url = await blobUrl(m, a);
    if (kind === "img") {
      box.innerHTML = `<img src="${url}" alt="${esc(a.name)}" title="${esc(a.name)}">`;
      box.querySelector("img").onclick = () => lightbox(url, a, m);
    } else {
      box.innerHTML = `<${kind} src="${url}" controls preload="metadata"></${kind}><button class="btn small att-save">Download</button>`;
      box.querySelector(".att-save").onclick = () => saveAttachment(m, a);
    }
  } catch (e) {
    box.innerHTML = `<div class="att-error">Couldn't load ${esc(a.name)}: ${esc(String(e))}</div><button class="btn small">Retry</button>`;
    box.querySelector("button").onclick = () => loadMedia(box, m, a, kind);
  }
}

function lightbox(url, a, m) {
  const root = $("#modal-root");
  root.innerHTML = `<div class="backdrop lightbox"><img src="${url}" alt=""><div class="lightbox-bar"><span>${esc(a.name)}</span><button class="btn" id="lb-save">Download</button></div></div>`;
  $(".backdrop", root).onclick = (e) => {
    if (e.target === e.currentTarget || e.target.tagName === "IMG") root.innerHTML = "";
  };
  $("#lb-save").onclick = () => saveAttachment(m, a);
}

async function saveAttachment(m, a) {
  const path = await call("save_attachment", { attachment: a, prefer: [m.author] });
  toast(`Saved to ${path}`);
}

async function toggleReaction(serverId, m, emoji) {
  rememberEmoji(emoji);
  const on = !(m.reactions?.[emoji] || []).includes(state.me.id);
  await call("react", { server: serverId, target: m.id, emoji, on });
}

function confirmDelete(serverId, m) {
  modal({
    title: "Delete message?",
    body: esc((m.body || "This message").slice(0, 200)),
    submit: "Delete",
    danger: true,
    onSubmit: () => call("delete", { server: serverId, target: m.id }),
  });
}

// ---------- composer ----------

let lastTypingSent = 0;

function wireComposer() {
  const box = $("#composer");
  box.onkeydown = async (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      await submitComposer();
    } else if (e.key === "Escape") {
      state.reply = state.editing = null;
      if (box.dataset.editing) box.value = "";
      delete box.dataset.editing;
      renderComposerBars();
    } else if (e.key === "ArrowUp" && !box.value) {
      const c = current();
      const mine = (state.messages[key(c.server, c.channel)] || []).filter((m) => m.author === state.me.id && !m.deleted);
      if (mine.length) {
        e.preventDefault();
        startEdit(mine[mine.length - 1]);
      }
    }
  };
  box.oninput = () => {
    box.style.height = "auto";
    box.style.height = Math.min(box.scrollHeight, window.innerHeight * 0.4) + "px";
    const c = current();
    if (c && box.value && Date.now() - lastTypingSent > 3000 && !state.editing) {
      lastTypingSent = Date.now();
      invoke("typing", { server: c.server, channel: c.channel }).catch(() => {});
    }
  };
  box.onpaste = (e) => {
    const files = [...(e.clipboardData?.files || [])];
    if (files.length) {
      e.preventDefault();
      addFiles(files);
    }
  };
  $("#attach-btn").onclick = () => $("#file-input").click();
  $("#file-input").onchange = (e) => {
    addFiles([...e.target.files]);
    e.target.value = "";
  };
  $("#emoji-btn").onclick = (e) => {
    e.stopPropagation();
    openEmojiPicker($("#emoji-btn"), insertAtCursor);
  };
  const main = $("#main");
  main.ondragover = (e) => {
    if (e.dataTransfer?.types.includes("Files")) {
      e.preventDefault();
      main.classList.add("dropping");
    }
  };
  main.ondragleave = (e) => {
    if (e.target === main || !main.contains(e.relatedTarget)) main.classList.remove("dropping");
  };
  main.ondrop = (e) => {
    e.preventDefault();
    main.classList.remove("dropping");
    addFiles([...(e.dataTransfer?.files || [])]);
  };
}

function insertAtCursor(text) {
  const box = $("#composer");
  if (!box) return;
  rememberEmoji(text);
  const { selectionStart: a, selectionEnd: b, value } = box;
  box.value = value.slice(0, a) + text + value.slice(b);
  box.selectionStart = box.selectionEnd = a + text.length;
  box.focus();
}

function startReply(m) {
  state.editing = null;
  state.reply = m;
  renderComposerBars();
  $("#composer")?.focus();
}

function startEdit(m) {
  state.reply = null;
  state.editing = m;
  const box = $("#composer");
  box.value = m.body;
  box.dataset.editing = "1";
  box.oninput?.();
  renderComposerBars();
  box.focus();
}

function renderComposerBars() {
  const bars = $("#composer-bars");
  if (!bars) return;
  let html = "";
  if (state.reply) html += `<div class="bar">Replying to <b>${esc(who(state.reply.author))}</b><button class="icon-btn" data-cancel-bar>✕</button></div>`;
  if (state.editing) html += `<div class="bar">Editing message · Esc to cancel<button class="icon-btn" data-cancel-bar>✕</button></div>`;
  if (state.pending.length) {
    html += `<div class="pending">${state.pending
      .map(
        (p) =>
          `<div class="pending-file${p.error ? " failed" : ""}">${p.preview ? `<img src="${p.preview}" alt="">` : `<span class="file-icon">📄</span>`}<div class="file-meta"><div class="file-name">${esc(p.name)}</div><div class="file-size">${p.error ? esc(p.error) : p.ready ? fmtSize(p.size) : "Encrypting…"}</div></div><button class="icon-btn" data-remove="${p.id}" title="Remove">✕</button></div>`,
      )
      .join("")}</div>`;
  }
  bars.innerHTML = html;
  bars.querySelectorAll("[data-cancel-bar]").forEach(
    (b) =>
      (b.onclick = () => {
        if (state.editing) $("#composer").value = "";
        state.reply = state.editing = null;
        renderComposerBars();
      }),
  );
  bars.querySelectorAll("[data-remove]").forEach(
    (b) =>
      (b.onclick = () => {
        state.pending = state.pending.filter((p) => p.id !== b.dataset.remove);
        renderComposerBars();
      }),
  );
}

let pendingSeq = 0;

async function addFiles(files) {
  for (const f of files) {
    if (state.pending.length >= 10) {
      toast("Up to 10 files per message", { error: true });
      break;
    }
    if (f.size > MAX_FILE) {
      toast(`${f.name} is larger than 100 MB`, { error: true });
      continue;
    }
    const p = { id: String(++pendingSeq), name: f.name, size: f.size, mime: f.type || "application/octet-stream", ready: false };
    if (p.mime.startsWith("image/")) p.preview = URL.createObjectURL(f);
    state.pending.push(p);
    renderComposerBars();
    try {
      const bytes = new Uint8Array(await f.arrayBuffer());
      p.dto = await invoke("upload", bytes, { headers: { "x-name": encodeURIComponent(f.name), "x-mime": encodeURIComponent(p.mime) } });
      // Locally we already have the plaintext: show it without a round trip.
      if (p.preview) mediaCache.set(p.dto.blob.hash, p.preview);
      p.ready = true;
    } catch (e) {
      p.error = String(e);
    }
    renderComposerBars();
  }
}

async function submitComposer() {
  const box = $("#composer");
  const c = current();
  if (!c?.channel) return;
  const body = expandShortcodes(box.value.trim());
  if (state.editing) {
    const m = state.editing;
    if (!body) return;
    await call("edit", { server: c.server, target: m.id, body });
    state.editing = null;
    delete box.dataset.editing;
    box.value = "";
    renderComposerBars();
    return;
  }
  if (state.pending.some((p) => !p.ready && !p.error)) {
    toast("Still encrypting your files…");
    return;
  }
  const attachments = state.pending.filter((p) => p.ready).map((p) => p.dto);
  if (!body && !attachments.length) return;
  const reply = state.reply?.id || null;
  box.value = "";
  box.style.height = "auto";
  try {
    await call("send", { server: c.server, channel: c.channel, body, replyTo: reply, attachments });
    state.pending = [];
    state.reply = null;
    renderComposerBars();
  } catch {
    box.value = body; // keep what they wrote
  }
}

function renderTyping() {
  const el = $("#typing");
  const c = current();
  if (!el || !c) return;
  const now = Date.now();
  const t = state.typing[key(c.server, c.channel)] || {};
  const names = Object.entries(t)
    .filter(([p, until]) => until > now && p !== state.me.id)
    .map(([p]) => who(p));
  el.textContent = !names.length
    ? ""
    : names.length === 1
      ? `${names[0]} is typing…`
      : names.length <= 3
        ? `${names.slice(0, -1).join(", ")} and ${names.at(-1)} are typing…`
        : "Several people are typing…";
}
setInterval(renderTyping, 1000);

// ---------- emoji picker ----------

function rememberEmoji(e) {
  const r = state.settings.recentEmoji.filter((x) => x !== e);
  r.unshift(e);
  state.settings.recentEmoji = r.slice(0, 24);
  saveSettings();
}

function openEmojiPicker(anchor, onPick) {
  closeEmojiPicker();
  const picker = document.createElement("div");
  picker.className = "emoji-picker";
  picker.innerHTML = `<input class="emoji-search" placeholder="Search emoji"><div class="emoji-tabs">${["🕘", ...CATEGORIES.map((c) => c.icon)].map((i, n) => `<button data-cat="${n - 1}">${i}</button>`).join("")}</div><div class="emoji-grid"></div>`;
  document.body.append(picker);
  const r = anchor.getBoundingClientRect();
  const top = Math.max(8, r.top - 360);
  picker.style.top = `${top}px`;
  picker.style.left = `${Math.max(8, Math.min(window.innerWidth - 340, r.right - 330))}px`;
  const grid = $(".emoji-grid", picker);
  const show = (items) => {
    grid.innerHTML = items.length ? items.map(([e, code]) => `<button title=":${code}:" data-e="${e}">${e}</button>`).join("") : `<p class="hint">No emoji found</p>`;
  };
  const showCat = (i) => show(i < 0 ? state.settings.recentEmoji.map((e) => [e, ""]) : CATEGORIES[i].items);
  showCat(state.settings.recentEmoji.length ? -1 : 0);
  $(".emoji-search", picker).oninput = (e) => show(searchEmoji(e.target.value));
  picker.querySelectorAll("[data-cat]").forEach((b) => (b.onclick = () => showCat(+b.dataset.cat)));
  grid.onclick = (e) => {
    const e_ = e.target.closest("[data-e]");
    if (!e_) return;
    onPick(e_.dataset.e);
    closeEmojiPicker();
  };
  picker.onclick = (e) => e.stopPropagation();
  $(".emoji-search", picker).focus();
  setTimeout(() => document.addEventListener("click", closeEmojiPicker, { once: true }));
}

function closeEmojiPicker() {
  $(".emoji-picker")?.remove();
}

// ---------- side panel (members / profile) ----------

function renderSide() {
  const el = $("#members");
  if (state.view === "home") {
    if (state.homeTab === "friends") {
      const active = state.friends.filter((f) => f.state === "friends" && state.online.has(f.peer));
      el.innerHTML = `<div class="section-label"><span>Active now</span></div>${
        active.length ? active.map((f) => `<div class="member on" data-dmpeer="${f.peer}">${avatar(f.peer, "md")}<div>${esc(who(f.peer))}</div></div>`).join("") : `<p class="hint">When friends are online, they'll show up here.</p>`
      }`;
      el.querySelectorAll("[data-dmpeer]").forEach((m) => (m.onclick = () => openDm(m.dataset.dmpeer)));
      return;
    }
    const s = server(state.homeTab);
    if (!s) return (el.innerHTML = "");
    const on = state.online.has(s.dm);
    el.innerHTML = `<div class="profile-card">${avatar(s.dm, "xl")}<h3>${esc(who(s.dm))}</h3><div class="fstatus">${on ? "Online" : "Offline"}</div><label>Peer ID</label>${idBox(s.dm)}<button class="btn danger" id="block-dm">Block</button></div>`;
    el.querySelectorAll("[data-copy]").forEach((b) => (b.onclick = () => copy(b.dataset.copy)));
    $("#block-dm").onclick = () => friendAction(s.dm, "block");
    return;
  }
  const s = server();
  if (!s) return (el.innerHTML = "");
  const on = (p) => p === state.me.id || state.online.has(p);
  const sorted = [...s.members].sort((a, b) => on(b) - on(a) || who(a).localeCompare(who(b)));
  const owner = isOwner(s);
  let html = `<div class="section-label"><span>Members — ${s.members.length}</span>${owner ? `<button class="icon-btn" id="invite-btn" title="Invite">+</button>` : ""}</div>`;
  for (const p of sorted) {
    const tag = p === s.owner ? `<div class="tag">owner</div>` : "";
    const f = friendOf(p);
    const actions = [];
    if (p !== state.me.id && (!f || f.state === "incoming")) actions.push(`<button class="icon-btn" data-addfriend="${p}" title="Add friend">👋</button>`);
    if (f?.state === "friends") actions.push(`<button class="icon-btn" data-dmpeer="${p}" title="Message">💬</button>`);
    if (owner && p !== state.me.id) actions.push(`<button class="icon-btn" data-kick="${p}" title="Remove from server">✕</button>`);
    const speaking = state.speaking.has(key(s.id, p)) ? "speaking" : "";
    const av = avatar(p, "md", speaking).replace(/<\/div>$/, `<span class="status${on(p) ? " on" : ""}"></span></div>`);
    html += `<div class="member${on(p) ? " on" : ""}" data-speak-server="${s.id}">${av}<div class="mname">${esc(who(p))}${tag}</div><div class="member-actions">${actions.join("")}</div></div>`;
  }
  if (s.hosts.length) {
    html += `<div class="section-label"><span>Storage nodes</span></div>`;
    for (const h of s.hosts) {
      html += `<div class="member${state.online.has(h) ? " on" : ""}"><div class="avatar md" style="background:#4e5058">⛁</div><div>${esc(h.slice(0, 10))}<div class="tag">blind node</div></div></div>`;
    }
  }
  el.innerHTML = html;
  $("#invite-btn")?.addEventListener("click", inviteModal);
  el.querySelectorAll("[data-addfriend]").forEach(
    (b) =>
      (b.onclick = async () => {
        await call("add_friend", { peer: b.dataset.addfriend });
        toast(`Friend request sent to ${who(b.dataset.addfriend)}`);
        await refreshFriends();
        renderSide();
      }),
  );
  el.querySelectorAll("[data-dmpeer]").forEach((b) => (b.onclick = () => openDm(b.dataset.dmpeer)));
  el.querySelectorAll("[data-kick]").forEach(
    (b) =>
      (b.onclick = () =>
        modal({
          title: `Remove ${who(b.dataset.kick)}?`,
          body: "They'll lose access to new messages. The server key is rotated so they can't read anything sent after this.",
          submit: "Remove",
          danger: true,
          onSubmit: () => call("kick", { server: s.id, peer: b.dataset.kick }),
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
      await openServer(id);
    },
  });
}

function createChannelModal(voice) {
  modal({
    title: voice ? "Create voice channel" : "Create text channel",
    fields: [{ name: "name", label: "Channel name", placeholder: voice ? "Hangout" : "new-channel" }],
    submit: "Create",
    onSubmit: async ({ name }) => {
      if (!name) throw new Error("Give the channel a name");
      const id = await call("create_channel", { server: state.view, name, voice });
      await refreshServers();
      if (!voice) await openChannel(id);
    },
  });
}

function inviteModal() {
  const s = server();
  const friends = state.friends.filter((f) => f.state === "friends" && !s.members.includes(f.peer));
  const extra = friends.length
    ? `<label>Invite a friend</label><div class="invite-friends">${friends.map((f) => `<button type="button" class="invite-friend" data-peer="${f.peer}">${avatar(f.peer, "sm")}<span>${esc(who(f.peer))}</span></button>`).join("")}</div>`
    : "";
  const root = modal({
    title: `Invite to ${s?.name}`,
    body: "Invite a friend, or paste anyone's peer ID. They'll get the invite as soon as you're both online.",
    extra,
    fields: [{ name: "peer", label: "Peer ID", placeholder: "64 characters" }],
    submit: "Invite",
    onSubmit: async ({ peer }) => {
      if (!peer) throw new Error("Paste a peer ID or pick a friend");
      await call("invite", { server: state.view, peer });
      toast("Invite sent");
    },
  });
  root.querySelectorAll(".invite-friend").forEach(
    (b) =>
      (b.onclick = async () => {
        await call("invite", { server: state.view, peer: b.dataset.peer });
        toast(`Invited ${who(b.dataset.peer)}`);
        b.disabled = true;
        b.classList.add("done");
      }),
  );
}

function serverMenu() {
  const s = server();
  const level = state.settings.notify[s.id] || "all";
  const fields = [
    {
      name: "notify",
      type: "select",
      label: "Notifications",
      value: level,
      options: [
        ["all", "All messages"],
        ["mentions", "Only @mentions"],
        ["none", "Nothing"],
      ],
    },
  ];
  if (isOwner(s)) fields.push({ name: "node", label: "Add a storage node (optional)", placeholder: "Node peer ID: keeps encrypted history for offline delivery" });
  const root = modal({
    title: s.name || "Server",
    extra: `<label>Server ID</label>${idBox(s.id)}<label>Key epoch</label><div class="copybox"><span>${s.epoch}: rotates whenever someone leaves or is removed</span></div>${isOwner(s) ? "" : `<button type="button" class="btn danger wide" id="leave-server">Leave server</button>`}`,
    fields,
    submit: "Save",
    onSubmit: async ({ notify: n, node }) => {
      state.settings.notify[s.id] = n;
      saveSettings();
      if (node) {
        await call("add_host", { server: s.id, peer: node });
        toast("Asked the node to host this server");
      }
    },
  });
  root.querySelector("#leave-server")?.addEventListener("click", () =>
    modal({
      title: `Leave ${s.name}?`,
      body: "You'll lose access to its channels and history on this device. Someone will have to invite you again to rejoin.",
      submit: "Leave",
      danger: true,
      onSubmit: async () => {
        await call("leave_server", { server: s.id });
        await refreshServers();
        await openHome();
      },
    }),
  );
}

function invitesModal() {
  const root = modal({
    title: "Invites",
    extra: state.invites
      .map(
        (i, n) =>
          `<div class="member on"><div>${esc(i.name)}<div class="tag">from ${esc(who(i.from))}</div></div><button type="button" class="btn primary" style="margin-left:auto" data-accept="${n}">Join</button></div>`,
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
  const root = modal({
    title: "Profile & settings",
    extra: `
      <div class="profile-edit">${avatar(state.me.id, "xl")}
        <div><button type="button" class="btn" id="avatar-pick">Change picture</button>
        ${state.avatars[state.me.id] ? `<button type="button" class="btn" id="avatar-clear">Remove</button>` : ""}
        <input type="file" id="avatar-file" accept="image/*" hidden></div>
      </div>
      <label>Your peer ID</label>${idBox(state.me.id)}<p style="margin-top:8px">Share this so people can add you. Your private key never leaves this device.</p>
      <button type="button" class="btn" id="devices-btn">🎙 Microphone, speakers & camera…</button>`,
    fields: [
      { name: "name", label: "Display name", value: state.me.name },
      { name: "dnd", type: "checkbox", label: "Do not disturb (no notifications or sounds)", value: state.settings.dnd },
      { name: "dial", label: "Connect to a peer (optional)", placeholder: "Peer ID" },
    ],
    submit: "Save",
    onSubmit: async ({ name, dnd, dial }) => {
      if (name && name !== state.me.name) {
        await call("set_name", { name });
        state.me.name = name;
      }
      state.settings.dnd = dnd;
      saveSettings();
      if (dial) await call("dial", { peer: dial });
      renderUser();
    },
  });
  $("#devices-btn", root).onclick = devicesModal;
  $("#avatar-pick", root).onclick = () => $("#avatar-file", root).click();
  $("#avatar-file", root).onchange = async (e) => {
    const f = e.target.files[0];
    if (!f) return;
    try {
      const png = await resizeImage(f, 256);
      await call("set_avatar", png);
      toast("Profile picture updated");
      root.innerHTML = "";
    } catch (err) {
      if (!err?.shown) toast(`Couldn't use that image: ${err}`, { error: true });
    }
  };
  $("#avatar-clear", root)?.addEventListener("click", async () => {
    await call("set_avatar", new Uint8Array());
    root.innerHTML = "";
  });
}

/** Center-crops an image to a square PNG of `size` pixels. */
async function resizeImage(file, size) {
  const img = await createImageBitmap(file);
  const side = Math.min(img.width, img.height);
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = size;
  canvas.getContext("2d").drawImage(img, (img.width - side) / 2, (img.height - side) / 2, side, side, 0, 0, size, size);
  const blob = await new Promise((r) => canvas.toBlob(r, "image/png"));
  return new Uint8Array(await blob.arrayBuffer());
}

// ---------- voice ----------

/** Picks the microphone, speakers and camera. Works mid-call. */
async function devicesModal() {
  const audio = await invoke("audio_devices").catch((e) => {
    toast(`Couldn't list audio devices: ${e}`, { error: true });
    return { inputs: [], outputs: [], input: null, output: null };
  });
  const cameras = await listCameras();
  const opts = (list, unnamed) => [["", "System default"], ...list.map((d, i) => [d.id, d.name || `${unnamed} ${i + 1}`])];
  // A saved device that's unplugged still shows, so saving doesn't silently forget it.
  const keep = (list, id, what) => (id && !list.some((d) => d.id === id) ? [...list, { id, name: `${what} (not connected)` }] : list);
  const camId = state.settings.camera || "";
  modal({
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

async function joinVoice(serverId, channel) {
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

async function leaveVoice() {
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

async function setMute(muted, deafened) {
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
      invoke(cfg.push, buf).catch(() => {});
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

async function startShare() {
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

async function stopShare() {
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
async function openCamera() {
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
  if (!state.camera) return;
  let stream;
  try {
    stream = await openCamera();
  } catch (e) {
    toast(`Couldn't open that camera: ${e.message || e.name}`, { error: true });
    return;
  }
  state.camera.stop();
  state.camera = await cameraFrom(stream);
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
const viewers = new Map();

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

function dropViewer(id) {
  const v = viewers.get(id);
  if (v?.decoder && v.decoder.state !== "closed") v.decoder.close();
  viewers.delete(id);
}

function onVideo(buf) {
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

function startWatching(peer) {
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

function stopWatching(peer) {
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

function renderCall() {
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

// ---------- updates ----------

// Ask GitHub for a newer release now and every few hours; offer a one-click update.
async function checkForUpdate() {
  try {
    const update = await invoke("check_update");
    if (update) {
      toast(`Hearth ${update.version} is available (you have ${update.current}).`, {
        action: installUpdate,
        actionLabel: "Update",
        timeout: 60_000,
      });
    }
  } catch (e) {
    console.warn("update check failed", e);
  }
  setTimeout(checkForUpdate, 6 * 60 * 60 * 1000);
}

async function installUpdate() {
  if (state.inVoice) await leaveVoice().catch(() => {});
  toast("Downloading update… Hearth will restart when it's done.", { timeout: 120_000 });
  await call("install_update");
}

// ---------- boot ----------

window.addEventListener("focus", () => {
  const c = current();
  if (c) {
    delete state.unread[key(c.server, c.channel)];
    delete state.mentions[key(c.server, c.channel)];
    renderNav();
    updateTitle();
  }
});

async function boot() {
  // Listen before loading anything, so no event slips between snapshot and stream.
  const backlog = [];
  let ready = false;
  await listen("hearth", (e) => (ready ? onUpdate(e.payload) : backlog.push(e.payload)));

  state.me = await invoke("me");
  state.names = await invoke("names");
  for (const p of await invoke("online")) state.online.add(p);
  await refreshFriends();
  for (const r of await invoke("voice_snapshot")) (state.voice[r.server] ||= {})[r.channel] = roomOf(r);
  const ch = new Channel();
  ch.onmessage = onVideo;
  await invoke("video_channel", { channel: ch });
  state.servers = await invoke("servers");
  const first = state.servers.find((s) => !s.dm);
  if (first) {
    state.view = first.id;
    state.channel = textChannel(first)?.id || null;
  }
  await refreshServers();
  // Fill DM previews so the list is ordered by recent activity.
  for (const s of state.servers.filter((x) => x.dm)) {
    const c = textChannel(s);
    if (c) state.messages[key(s.id, c.id)] = await invoke("history", { server: s.id, channel: c.id, limit: 200 });
  }
  render();
  loadAvatars();

  ready = true;
  for (const u of backlog) await onUpdate(u);
  checkForUpdate();
}

boot().catch((e) => {
  document.body.innerHTML = `<div class="empty"><h2>Hearth couldn't start</h2><p>${esc(e)}</p></div>`;
});
