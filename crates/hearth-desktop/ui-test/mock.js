// A stateful stand-in for the Rust backend, plus scripted user flows that use
// real clicks, so UI regressions (like a modal that never submits) fail CI.

const results = [];
const calls = [];
const errors = [];
window.addEventListener("error", (e) => errors.push(String(e.message)));
document.addEventListener("securitypolicyviolation", (e) => errors.push(`CSP blocked ${e.blockedURI} (${e.violatedDirective})`));
window.addEventListener("unhandledrejection", (e) => errors.push("unhandled rejection: " + String(e.reason)));

const ME = "a".repeat(64);
const BOB = "b".repeat(64);
const CAROL = "c".repeat(64);
let listener = null;
let seq = 10;

const DEN = "d".repeat(64);
const DEN_GENERAL = `${ME}:2`;
const DEN_VOICE = `${ME}:3`;
const DM = "f".repeat(64);
const DM_TEXT = `${BOB}:2`;
const DM_VOICE = `${BOB}:3`;
const PIXEL = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]); // not a real PNG; enough to make a blob URL

const db = {
  servers: [
    {
      id: DEN, name: "The Den", owner: ME, members: [ME, BOB, CAROL], hosts: [], epoch: 0, dm: null,
      channels: [{ id: DEN_GENERAL, name: "general", voice: false }, { id: DEN_VOICE, name: "General", voice: true }],
    },
  ],
  messages: {
    [DEN_GENERAL]: [msg(BOB, DEN_GENERAL, "welcome to the den")],
  },
  friends: [],
};

function msg(author, channel, body, extra = {}) {
  return { id: `${author}:${++seq}`, channel, author, body, sent_at_ms: Date.now() - 1000, reply_to: null, attachments: [], edited: false, deleted: false, reactions: {}, ...extra };
}

function srv(id) {
  return db.servers.find((s) => s.id === id);
}

function emit(payload) {
  listener?.({ payload });
}

let statTicks = 0;
let scans = 0;

const handlers = {
  me: () => ({ id: ME, name: "neo" }),
  names: () => ({ [BOB]: "bob", [CAROL]: "carol" }),
  online: () => [BOB],
  friends: () => structuredClone(db.friends),
  // Bob was already in voice before this window opened: the snapshot must show him.
  voice_snapshot: () => [{ server: DEN, channel: DEN_VOICE, peers: [BOB], live: [], camera: [], muted: [BOB], deafened: [] }],
  avatars: () => ({}),
  video_channel: ({ channel }) => {
    window.__video = channel;
  },
  set_camera: () => null,
  audio_devices: () => ({
    inputs: [{ id: "pulseaudio:usb_mic", name: "USB Mic" }, { id: "pulseaudio:builtin_mic", name: "Built-in Mic" }],
    outputs: [{ id: "pulseaudio:headset", name: "Headset" }],
    input: null,
    output: "pulseaudio:headset",
  }),
  set_audio_devices: () => [],
  call_stats: () => {
    statTicks++;
    return {
      input_db: -20, transmitting: true, sent_packets: 50 * statTicks, sent_bytes: 8000 * statTicks,
      peers: [{ peer: BOB, rtt_ms: 42, relay: false, received: 25 * statTicks, received_bytes: 4000 * statTicks, lost: statTicks, late: 0, jitter_ms: 1.5, buffered_ms: 40, level_db: -30 }],
    };
  },
  my_link: () => `hearth://friend/${ME}`,
  invite_link: ({ server }) => `hearth://join/${server}/${ME}/${"ab".repeat(16)}`,
  qr_svg: ({ text }) => `<svg xmlns="http://www.w3.org/2000/svg" data-text="${text}" width="10" height="10"><rect width="10" height="10"/></svg>`,
  // The camera "sees" a friend code on the third frame.
  scan_qr: () => (++scans >= 3 ? `hearth://friend/${CAROL}` : null),
  revoke_invite_links: () => 2,
  open_link: ({ link }) => (link.includes("/join/") ? "join" : "friend"),
  set_live: () => null,
  watch: () => null,
  // Loop our camera back as Bob's, so the receive path (decode -> tile) is exercised too.
  push_camera: (bytes) => {
    const out = new Uint8Array(33 + bytes.length);
    out[0] = 3;
    out.set(Uint8Array.from(BOB.match(/../g).map((h) => parseInt(h, 16))), 1);
    out.set(bytes, 33);
    window.__video?.onmessage?.(out.buffer);
    return 1;
  },
  push_video: () => window.__viewers ?? 1,
  servers: () => structuredClone(db.servers),
  history: ({ channel }) => structuredClone(db.messages[channel] || []),
  create_server: ({ name }) => {
    const id = String(db.servers.length + 1).repeat(64).slice(0, 64);
    db.servers.push({
      id, name, owner: ME, members: [ME], hosts: [], epoch: 0, dm: null,
      channels: [{ id: `${ME}:${++seq}`, name: "general", voice: false }, { id: `${ME}:${++seq}`, name: "General", voice: true }],
    });
    return id;
  },
  create_channel: ({ server, name, voice }) => {
    const id = `${ME}:${++seq}`;
    srv(server).channels.push({ id, name, voice });
    return id;
  },
  send: ({ server, channel, body, replyTo, attachments }) => {
    const message = msg(ME, channel, body, { reply_to: replyTo || null, attachments: attachments || [], sent_at_ms: Date.now() });
    (db.messages[channel] ||= []).push(message);
    emit({ type: "message", server, message });
    return message.id;
  },
  edit: ({ server, target, body }) => {
    for (const list of Object.values(db.messages)) {
      const m = list.find((x) => x.id === target);
      if (m) {
        Object.assign(m, { body, edited: true });
        emit({ type: "message_updated", server, message: structuredClone(m) });
      }
    }
  },
  delete: ({ server, target }) => {
    for (const list of Object.values(db.messages)) {
      const m = list.find((x) => x.id === target);
      if (m) {
        Object.assign(m, { body: "", deleted: true, attachments: [], reactions: {} });
        emit({ type: "message_updated", server, message: structuredClone(m) });
      }
    }
  },
  react: ({ server, target, emoji, on }) => {
    for (const list of Object.values(db.messages)) {
      const m = list.find((x) => x.id === target);
      if (!m) continue;
      const who = (m.reactions[emoji] ||= []);
      if (on && !who.includes(ME)) who.push(ME);
      if (!on) m.reactions[emoji] = who.filter((p) => p !== ME);
      if (!m.reactions[emoji].length) delete m.reactions[emoji];
      emit({ type: "message_updated", server, message: structuredClone(m) });
    }
  },
  typing: () => null,
  upload: (bytes, options) => ({
    name: decodeURIComponent(options.headers["x-name"]),
    mime: decodeURIComponent(options.headers["x-mime"]),
    blob: { hash: "9".repeat(64), key: "8".repeat(64), size: bytes.length + 16 },
  }),
  fetch_blob: () => PIXEL.buffer,
  save_attachment: () => "/home/neo/Downloads/file.bin",
  notify: () => null,
  open_url: () => null,
  invite: () => null,
  kick: ({ server, peer }) => {
    srv(server).members = srv(server).members.filter((m) => m !== peer);
  },
  add_friend: ({ peer }) => {
    const f = db.friends.find((x) => x.peer === peer);
    if (f) {
      f.state = "friends";
      f.dm = DM;
      if (!srv(DM)) {
        db.servers.push({
          id: DM, name: "", owner: BOB, members: [BOB, ME], hosts: [], epoch: 0, dm: BOB,
          channels: [{ id: DM_TEXT, name: "general", voice: false }, { id: DM_VOICE, name: "General", voice: true }],
        });
      }
    } else db.friends.push({ peer, state: "outgoing", since_ms: Date.now(), dm: null, online: true });
  },
  remove_friend: ({ peer }) => {
    db.friends = db.friends.filter((f) => f.peer !== peer);
  },
  block: () => null,
  set_name: () => null,
  dial: () => null,
  voice_join: () => [],
  voice_leave: () => null,
  set_mute: () => null,
  voice_levels: () => ({ input_db: -60, transmitting: false }),
  check_update: () => ({ version: "9.9.9", current: "0.2.0", notes: null }),
  install_update: () => null,
};

window.__TAURI__ = {
  core: {
    invoke: async (cmd, args = {}, options = {}) => {
      calls.push({ cmd, args, options });
      if (!(cmd in handlers)) throw `mock: unexpected command ${cmd}`;
      return handlers[cmd](args, options);
    },
    Channel: class {
      constructor() {
        this.onmessage = null;
      }
    },
  },
  event: { listen: async (_name, cb) => (listener = cb) },
};

// A synthetic camera/screen: an animated canvas, so capture -> encode -> decode runs for real.
function fakeStream() {
  const c = document.createElement("canvas");
  c.width = 640;
  c.height = 360;
  const ctx = c.getContext("2d");
  let t = 0;
  setInterval(() => {
    ctx.fillStyle = `hsl(${(t += 7) % 360} 70% 50%)`;
    ctx.fillRect(0, 0, 640, 360);
    ctx.fillStyle = "#fff";
    ctx.fillRect((t * 3) % 600, 150, 40, 40);
  }, 33);
  return c.captureStream(30);
}
const cameraRequests = [];
navigator.mediaDevices.getUserMedia = async (c) => {
  cameraRequests.push(c);
  return fakeStream();
};
navigator.mediaDevices.enumerateDevices = async () => [
  { kind: "videoinput", deviceId: "obs-virtual", label: "OBS Virtual Camera" },
  { kind: "videoinput", deviceId: "webcam-1", label: "HD Webcam" },
  { kind: "audioinput", deviceId: "x", label: "ignored: audio is native" },
];
navigator.mediaDevices.getDisplayMedia = async () => fakeStream();

// ---- helpers ----

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const q = (sel) => document.querySelector(sel);
const qa = (sel) => [...document.querySelectorAll(sel)];

async function until(what, fn, timeout = 2500) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    if (fn()) return;
    await sleep(20);
  }
  throw new Error(`timed out waiting for ${what}${errors.length ? ` (errors: ${errors.join("; ")})` : ""}`);
}

function called(cmd) {
  return calls.filter((c) => c.cmd === cmd);
}

async function fillModal(values, button = ".modal button.primary, .modal button.danger") {
  await until("modal", () => q(".modal"));
  for (const [name, value] of Object.entries(values)) {
    const input = q(`.modal [name="${name}"]`);
    if (input.type === "checkbox") input.checked = value;
    else input.value = value;
  }
  q(button).click(); // a real click, which is how users submit
}

function typeAndEnter(text) {
  const box = q("#composer");
  box.value = text;
  box.dispatchEvent(new Event("input", { bubbles: true }));
  box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
}

function lastMsgRow() {
  return qa("#messages .msg").at(-1);
}

async function test(name, fn) {
  try {
    await fn();
    results.push({ name, ok: true });
  } catch (e) {
    results.push({ name, ok: false, error: String(e.message || e) });
    q("#modal-root").innerHTML = ""; // don't cascade into the next test
    document.querySelector(".emoji-picker")?.remove();
  }
}

// ---- flows ----

async function run() {
  await until("app boot", () => called("servers").length > 0 && q("#server-header")?.textContent.includes("The Den"));

  await test("voice presence from before the window opened", async () => {
    await until("bob listed in voice", () => q(".voice-users")?.textContent.includes("bob"));
  });

  await test("speaking ring shows for others in voice", async () => {
    emit({ type: "speaking", server: DEN, peer: BOB, speaking: true });
    await until("ring on", () => q(`.voice-users .avatar[data-peer="${BOB}"]`)?.classList.contains("speaking"));
    emit({ type: "speaking", server: DEN, peer: BOB, speaking: false });
    await until("ring off", () => !q(`.voice-users .avatar[data-peer="${BOB}"]`)?.classList.contains("speaking"));
  });

  await test("typing indicator", async () => {
    emit({ type: "typing", server: DEN, channel: DEN_GENERAL, peer: BOB });
    await until("typing shown", () => q("#typing")?.textContent.includes("bob is typing"));
  });

  await test("send a message with Enter", async () => {
    typeAndEnter("hello hearth");
    await until("send call", () => called("send").some((c) => c.args.body === "hello hearth"));
    await until("message rendered", () => q("#messages").textContent.includes("hello hearth"));
    if (q("#composer").value !== "") throw new Error("composer not cleared");
  });

  await test("shortcodes become emoji", async () => {
    typeAndEnter("this is :fire:");
    await until("expanded", () => called("send").some((c) => c.args.body === "this is 🔥"));
  });

  await test("emoji picker inserts into the composer", async () => {
    q("#emoji-btn").click();
    await until("picker", () => q(".emoji-picker"));
    q(".emoji-search").value = "pizza";
    q(".emoji-search").dispatchEvent(new Event("input"));
    await until("result", () => q('.emoji-grid [data-e="🍕"]'));
    q('.emoji-grid [data-e="🍕"]').click();
    await until("inserted", () => q("#composer").value.includes("🍕"));
    q("#composer").value = "";
  });

  await test("reply to a message", async () => {
    const row = qa("#messages .msg").find((r) => r.textContent.includes("welcome to the den"));
    row.querySelector('[data-tool="reply"]').click();
    await until("reply bar", () => q("#composer-bars").textContent.includes("Replying to"));
    typeAndEnter("thanks!");
    await until("reply sent", () => called("send").some((c) => c.args.body === "thanks!" && c.args.replyTo?.startsWith(BOB)));
    await until("reply ref rendered", () => lastMsgRow().querySelector(".reply-ref")?.textContent.includes("bob"));
  });

  await test("react through the picker, then remove it", async () => {
    const row = qa("#messages .msg").find((r) => r.textContent.includes("welcome to the den"));
    row.querySelector('[data-tool="react"]').click();
    await until("picker", () => q(".emoji-picker"));
    q(".emoji-search").value = "thumbsup";
    q(".emoji-search").dispatchEvent(new Event("input"));
    await until("thumbs", () => q('.emoji-grid [data-e="👍"]'));
    q('.emoji-grid [data-e="👍"]').click();
    await until("react on", () => called("react").some((c) => c.args.emoji === "👍" && c.args.on));
    await until("chip shown", () => qa(".reaction.mine").some((b) => b.textContent.includes("👍")));
    qa(".reaction.mine").find((b) => b.textContent.includes("👍")).click();
    await until("react off", () => called("react").some((c) => c.args.emoji === "👍" && !c.args.on));
  });

  await test("edit your own message", async () => {
    const row = qa("#messages .msg").find((r) => r.textContent.includes("hello hearth"));
    row.querySelector('[data-tool="edit"]').click();
    await until("composer filled", () => q("#composer").value === "hello hearth");
    typeAndEnter("hello hearth!!");
    await until("edit call", () => called("edit").some((c) => c.args.body === "hello hearth!!"));
    await until("edited marker", () => q("#messages").textContent.includes("(edited)"));
  });

  await test("delete a message", async () => {
    const row = qa("#messages .msg").find((r) => r.textContent.includes("hello hearth!!"));
    row.querySelector('[data-tool="delete"]').click();
    await fillModal({});
    await until("delete call", () => called("delete").length === 1);
    await until("shown as deleted", () => q("#messages").textContent.includes("Message deleted"));
  });

  await test("attach and send a file", async () => {
    const dt = new DataTransfer();
    dt.items.add(new File([new Uint8Array(1234)], "notes.pdf", { type: "application/pdf" }));
    const input = q("#file-input");
    input.files = dt.files;
    input.dispatchEvent(new Event("change"));
    await until("upload call", () => called("upload").some((c) => c.options.headers?.["x-name"] === "notes.pdf" && c.args.length === 1234));
    await until("ready", () => q(".pending-file")?.textContent.includes("KB"));
    typeAndEnter("here you go");
    await until("sent with attachment", () => called("send").some((c) => c.args.attachments?.[0]?.name === "notes.pdf"));
    await until("file card", () => q(".att-file")?.textContent.includes("notes.pdf"));
    q(".att-file .att-save").click();
    await until("save call", () => called("save_attachment").length === 1);
  });

  await test("incoming image loads inline", async () => {
    const image = { name: "cat.png", mime: "image/png", blob: { hash: "7".repeat(64), key: "6".repeat(64), size: 100 } };
    emit({ type: "message", server: DEN, message: msg(BOB, DEN_GENERAL, "", { attachments: [image] }) });
    await until("fetched", () => called("fetch_blob").some((c) => c.args.blob.hash === image.blob.hash && c.args.prefer[0] === BOB));
    await until("img shown", () => q('.att-image img[alt="cat.png"]'));
  });

  await test("incoming video plays inline", async () => {
    const clip = { name: "clip.webm", mime: "video/webm", blob: { hash: "8".repeat(64), key: "6".repeat(64), size: 100 } };
    emit({ type: "message", server: DEN, message: msg(BOB, DEN_GENERAL, "", { attachments: [clip] }) });
    await until("load button", () => q(".att-video .att-load"));
    q(".att-video .att-load").click();
    await until("video element", () => q('.att-video video[src^="blob:"]'));
    await sleep(300); // a CSP block would be reported by now
  });

  await test("links open in the browser", async () => {
    emit({ type: "message", server: DEN, message: msg(BOB, DEN_GENERAL, "see https://example.com/page") });
    await until("link", () => q('a[data-url="https://example.com/page"]'));
    q('a[data-url="https://example.com/page"]').click();
    await until("open_url", () => called("open_url").some((c) => c.args.url === "https://example.com/page"));
  });

  await test("long channels draw only the latest messages", async () => {
    const old = msg(BOB, DEN_GENERAL, "from last week", { sent_at_ms: Date.now() - 7 * 86_400_000 });
    db.messages[DEN_GENERAL].unshift(old);
    for (let i = 0; i < 160; i++) emit({ type: "message", server: DEN, message: msg(CAROL, DEN_GENERAL, `spam ${i}`) });
    await until("last one drawn", () => lastMsgRow()?.textContent.includes("spam 159"));
    const drawn = qa("#messages .msg").length;
    if (drawn > 150) throw new Error(`${drawn} messages drawn`);
    if (!q("#messages .day-divider")?.textContent.includes("Today")) throw new Error("no day divider");
    q("[data-older]").click();
    await until("older drawn", () => qa("#messages .msg").length > drawn);
  });

  await test("messages elsewhere notify and mark unread", async () => {
    q('[data-new="false"]').click();
    await fillModal({ name: "memes" });
    await until("memes open", () => q(".channel.active")?.textContent.includes("memes"));
    emit({ type: "message", server: DEN, message: msg(BOB, DEN_GENERAL, "psst @neo") });
    await until("notified", () => called("notify").some((c) => c.args.title.includes("bob") && c.args.body === "psst @neo"));
    await until("unread", () => qa(".channel.unread").some((b) => b.textContent.includes("general")));
    await until("mention badge", () => qa(".channel .count").length > 0);
  });

  await test("friend request: notification, accept, DM", async () => {
    db.friends.push({ peer: BOB, state: "incoming", since_ms: Date.now(), dm: null, online: true });
    emit({ type: "friend", peer: BOB, request: true });
    await until("notified", () => called("notify").some((c) => c.args.title === "Friend request"));
    q(".rail-btn.home").click();
    await until("friends page", () => q(".friends-page"));
    q('[data-tab="pending"]').click();
    await until("pending row", () => q('.friend-row [data-act="accept"]'));
    q('.friend-row [data-act="accept"]').click();
    await until("add_friend", () => called("add_friend").some((c) => c.args.peer === BOB));
    await until("dm listed", () => q(`[data-dm="${DM}"]`));
    q(`[data-dm="${DM}"]`).click();
    await until("dm open", () => q("#composer")?.placeholder === "Message @bob");
    typeAndEnter("hey bob");
    await until("dm sent", () => called("send").some((c) => c.args.server === DM && c.args.body === "hey bob"));
  });

  await test("add a friend by ID", async () => {
    q("#friends-tab").click();
    q('[data-tab="add"]').click();
    await until("form", () => q("#add-friend-form"));
    q('#add-friend-form [name="peer"]').value = CAROL;
    q("#add-friend-form button").click();
    await until("request sent", () => called("add_friend").some((c) => c.args.peer === CAROL));
  });

  await test("friend QR code shows; scanning one sends a request", async () => {
    await until("moved to pending", () => q('[data-tab="pending"].active'));
    q('[data-tab="add"]').click();
    await until("my code", () => q("[data-qr-me] svg")?.dataset.text === `hearth://friend/${ME}`);
    q("[data-scan]").click();
    await until("scanner", () => q(".scanner video"));
    await until("scanned and used", () => called("open_link").some((c) => c.args.link === `hearth://friend/${CAROL}`), 6000);
    await until("scanner closed", () => !q(".scanner"));
    if (!called("scan_qr")[0].args.length || called("scan_qr")[0].args.length < 8) throw new Error("no frame sent to the QR reader");
  });

  await test("create server from the rail", async () => {
    q(".rail-btn.add").click();
    await fillModal({ name: "Second" });
    await until("second server", () => q("#server-header").textContent.includes("Second"));
  });

  await test("empty channel name shows an error", async () => {
    const before = called("create_channel").length;
    q('[data-new="false"]').click();
    await fillModal({ name: "" });
    await until("error toast", () => qa(".toast.error").some((t) => t.textContent.includes("name")));
    if (called("create_channel").length !== before) throw new Error("created a channel with no name");
    q("[data-cancel]").click();
  });

  await test("save settings", async () => {
    q("#settings").click();
    await fillModal({ name: "neo2" });
    await until("set_name call", () => called("set_name").some((c) => c.args.name === "neo2"));
    await until("name shown", () => q("#user-panel").textContent.includes("neo2"));
  });

  await test("backdrop click closes a modal", async () => {
    q("#settings").click();
    await until("modal", () => q(".modal"));
    q(".backdrop").click();
    await until("closed", () => !q(".modal"));
  });

  await test("join and leave voice", async () => {
    [...document.querySelectorAll(".rail-btn")].find((b) => b.title === "The Den").click();
    await until("den", () => q("#server-header").textContent.includes("The Den"));
    q("[data-voice]").click();
    await until("voice_join call", () => called("voice_join").length === 1);
    await until("voice panel", () => !q("#voice-panel").hidden);
    q("#hangup").click();
    await until("voice_leave call", () => called("voice_leave").length === 1);
  });

  await test("call view: tiles, others' mute state, controls", async () => {
    q("[data-voice]").click();
    await until("call view", () => q("#call.full .tiles"));
    await until("bob and me tiles", () => q(`.tile[data-peer="${BOB}"]`) && q(`.tile[data-peer="${ME}"]`));
    await until("bob shown muted", () => q(`.tile[data-peer="${BOB}"] .tile-label`)?.textContent.includes("🔇"));
    for (const c of ["mute", "deafen", "camera", "share", "leave"]) if (!q(`[data-ctl="${c}"]`)) throw new Error(`no ${c} control`);
    emit({ type: "speaking", server: DEN, peer: BOB, speaking: true });
    await until("speaking border", () => q(`.tile[data-peer="${BOB}"]`).classList.contains("speaking"));
  });

  await test("camera: own preview, frames out, peer video in", async () => {
    emit({ type: "voice", server: DEN, channel: DEN_VOICE, peers: [BOB, ME], live: [], camera: [BOB, ME], muted: [], deafened: [] });
    q('[data-ctl="camera"]').click();
    await until("set_camera on", () => called("set_camera").some((c) => c.args.on));
    await until("own preview", () => q(`.tile[data-peer="${ME}"] video.mirror`));
    await until("keyframe pushed", () => called("push_camera").some((c) => c.args[0] & 1), 6000);
    await until("bob's camera decoded", () => q(`.tile[data-peer="${BOB}"] canvas`)?.width > 0, 8000);
  });

  await test("camera falls back to VP8 where H.264 can't encode", async () => {
    q('[data-ctl="camera"]').click();
    await until("camera off", () => called("set_camera").some((c) => !c.args.on));
    const orig = VideoEncoder.isConfigSupported.bind(VideoEncoder);
    VideoEncoder.isConfigSupported = async (c) => (c.codec.startsWith("avc1") ? { supported: false } : orig(c));
    try {
      const before = called("push_camera").length;
      q('[data-ctl="camera"]').click();
      await until("VP8 keyframe out", () => called("push_camera").slice(before).some((c) => c.args[0] & 1 && c.args[1] === 2), 6000);
      // The loopback receiver switches its decoder from H.264 to VP8 and keeps drawing.
      const canvas = q(`.tile[data-peer="${BOB}"] canvas`);
      const pixel = () => Array.from(canvas.getContext("2d").getImageData(10, 10, 1, 1).data).join();
      const first = pixel();
      await until("bob's VP8 video updates", () => pixel() !== first, 6000);
    } finally {
      VideoEncoder.isConfigSupported = orig;
    }
  });

  await test("pick mic, speakers and camera mid-call", async () => {
    const cameraCalls = called("set_camera").length;
    q('[data-ctl="devices"]').click();
    await until("device modal", () => q('.modal [name="camera"]'));
    const labels = (n) => [...q(`.modal [name="${n}"]`).options].map((o) => o.textContent);
    if (!labels("camera").includes("HD Webcam") || !labels("camera").includes("OBS Virtual Camera")) throw new Error(`cameras: ${labels("camera")}`);
    if (q('.modal [name="output"]').value !== "pulseaudio:headset") throw new Error("saved speakers not preselected");
    if (!labels("input").includes("USB Mic")) throw new Error(`mics: ${labels("input")}`);
    const before = cameraRequests.length;
    await fillModal({ input: "pulseaudio:usb_mic", camera: "webcam-1" });
    await until("audio switched", () => called("set_audio_devices").some((c) => c.args.input === "pulseaudio:usb_mic" && c.args.output === "pulseaudio:headset"));
    await until("webcam opened", () => cameraRequests.slice(before).some((c) => c.video?.deviceId?.exact === "webcam-1"));
    await until("saved and closed", () => !q(".modal"), 6000);
    await until("still sending", () => q(`.tile[data-peer="${ME}"] video.mirror`));
    if (called("set_camera").length !== cameraCalls) throw new Error("camera blinked off for others while switching");
    if (JSON.parse(localStorage.getItem("hearth.settings")).camera !== "webcam-1") throw new Error("camera choice not saved");
    // The next time the camera starts, it's the chosen one.
    q('[data-ctl="camera"]').click();
    await until("camera off", () => called("set_camera").length > cameraCalls);
    const again = cameraRequests.length;
    q('[data-ctl="camera"]').click();
    await until("reopened chosen", () => cameraRequests.slice(again).some((c) => c.video?.deviceId?.exact === "webcam-1"));
  });

  await test("live call quality graph", async () => {
    q('[data-ctl="devices"]').click();
    await until("stats row", () => q(".stats-table td")?.textContent.includes("bob"), 4000);
    await until("ping and path", () => q(".stats-table").textContent.includes("42 ms") && q(".stats-table").textContent.includes("direct"), 4000);
    await until("loss shown", () => /\d\.\d%/.test(q(".stats-table").textContent));
    await until("mic meter moves", () => parseFloat(q(".meter-fill").style.width) > 0);
    if (!(q('.stats-graphs canvas[data-metric="rtt"]').width > 0)) throw new Error("graph not drawn");
    q("[data-cancel]").click();
    const n = called("call_stats").length;
    await sleep(1200);
    if (called("call_stats").length > n + 1) throw new Error("kept polling after the modal closed");
  });

  await test("focus a tile", async () => {
    q(`.tile[data-peer="${BOB}"]`).click();
    await until("spotlight", () => q(".tiles.spotlight .tile.big")?.dataset.peer === BOB);
    q(".tiles.spotlight .tile.big").click();
    await until("grid again", () => !q(".tiles.spotlight"));
  });

  await test("screen share shows your own preview", async () => {
    emit({ type: "voice", server: DEN, channel: DEN_VOICE, peers: [BOB, ME], live: [ME], camera: [BOB, ME], muted: [], deafened: [] });
    q('[data-ctl="share"]').click();
    await until("set_live", () => called("set_live").some((c) => c.args.live));
    await until("own screen tile", () => q(`.tile[data-tile="2|${ME}"] video`));
    q('[data-ctl="share"]').click();
    await until("stopped", () => called("set_live").some((c) => !c.args.live));
  });

  await test("screen share stops encoding while nobody watches", async () => {
    window.__viewers = 0;
    try {
      q('[data-ctl="share"]').click();
      await until("sharing", () => called("set_live").filter((c) => c.args.live).length >= 2);
      await until("first frame", () => called("push_video").length > 0, 4000);
      await sleep(600);
      const before = called("push_video").length;
      await sleep(1500);
      const idleFrames = called("push_video").length - before;
      if (idleFrames > 2) throw new Error(`encoded ${idleFrames} frames for nobody`);
      // Someone starts watching: frames flow again at once.
      window.__viewers = 1;
      emit({ type: "keyframe_requested", camera: false });
      const woke = called("push_video").length;
      await until("frames flowing", () => called("push_video").length > woke + 10, 4000);
    } finally {
      window.__viewers = 1;
      q('[data-ctl="share"]').click();
      await until("stopped", () => called("set_live").filter((c) => !c.args.live).length >= 2);
    }
  });

  await test("leave the call from the controls", async () => {
    q('[data-ctl="leave"]').click();
    await until("voice_leave", () => called("voice_leave").length >= 2);
    await until("camera off", () => called("set_camera").some((c) => !c.args.on));
  });

  await test("DM call area shows and joins", async () => {
    emit({ type: "voice", server: DM, channel: DM_VOICE, peers: [BOB], live: [], camera: [], muted: [], deafened: [] });
    q(".rail-btn.home").click();
    await until("dm entry", () => q(`[data-dm="${DM}"]`));
    q(`[data-dm="${DM}"]`).click();
    await until("call area", () => q("#call.compact:not([hidden]) .tile"));
    q('[data-ctl="join"]').click();
    await until("joined dm call", () => called("voice_join").some((c) => c.args.server === DM));
    await until("controls", () => q('#call.compact [data-ctl="leave"]'));
    q('#call.compact [data-ctl="leave"]').click();
    [...document.querySelectorAll(".rail-btn")].find((b) => b.title === "The Den").click();
    await until("den", () => q("#server-header").textContent.includes("The Den"));
  });

  await test("invite a peer", async () => {
    q("#invite-btn").click();
    await until("invite QR", () => q("#invite-qr svg")?.dataset.text.startsWith("hearth://join/"));
    await until("invite link", () => q("#invite-link").textContent.includes("hearth://join/"));
    await fillModal({ peer: "e".repeat(64) });
    await until("invite call", () => called("invite").length === 1);
  });

  await test("joining by code opens the server", async () => {
    emit({ type: "joined", server: DEN });
    await until("in the den", () => q("#server-header")?.textContent.includes("The Den"));
  });

  await test("owner can cancel invite codes", async () => {
    q("#server-menu").click();
    await until("menu", () => q("#revoke-codes"));
    q("#revoke-codes").click();
    await until("cancelled", () => qa(".toast").some((t) => t.textContent.includes("Cancelled 2 invite codes")));
    q("[data-cancel]").click();
  });

  await test("kick a member", async () => {
    q(`[data-kick="${CAROL}"]`).click();
    await fillModal({});
    await until("kick call", () => called("kick").some((c) => c.args.peer === CAROL));
  });

  await test("update offer installs on click", async () => {
    await until("update toast", () => qa(".toast").some((t) => t.textContent.includes("9.9.9")));
    qa(".toast").find((t) => t.textContent.includes("9.9.9")).querySelector("button").click();
    await until("install_update call", () => called("install_update").length === 1);
  });

  if (errors.length) results.push({ name: "no uncaught errors", ok: false, error: errors.join("; ") });
  document.body.dataset.results = JSON.stringify(results);
}

window.addEventListener("load", () =>
  run().catch((e) => {
    results.push({ name: "harness", ok: false, error: String(e) });
    document.body.dataset.results = JSON.stringify(results);
  }),
);
