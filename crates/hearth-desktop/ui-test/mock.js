// A stateful stand-in for the Rust backend, plus scripted user flows that use
// real clicks, so UI regressions (like a modal that never submits) fail CI.

const results = [];
const calls = [];
const errors = [];
window.addEventListener("error", (e) => errors.push(String(e.message)));
window.addEventListener("unhandledrejection", (e) => errors.push("unhandled rejection: " + String(e.reason)));

const ME = "a".repeat(64);
const BOB = "b".repeat(64);
let listener = null;
let seq = 10;
const db = { servers: [], messages: {} };

function srv(id) {
  return db.servers.find((s) => s.id === id);
}

const handlers = {
  me: () => ({ id: ME, name: "neo" }),
  names: () => ({ [BOB]: "bob" }),
  online: () => [BOB],
  video_channel: () => null,
  servers: () => structuredClone(db.servers),
  history: ({ channel }) => db.messages[channel] || [],
  create_server: ({ name }) => {
    const id = String(db.servers.length + 1).repeat(64).slice(0, 64);
    db.servers.push({
      id, name, owner: ME, members: [ME, BOB], hosts: [], epoch: 0,
      channels: [
        { id: `${ME}:${++seq}`, name: "general", voice: false },
        { id: `${ME}:${++seq}`, name: "General", voice: true },
      ],
    });
    return id;
  },
  create_channel: ({ server, name, voice }) => {
    const id = `${ME}:${++seq}`;
    srv(server).channels.push({ id, name, voice });
    return id;
  },
  send: ({ server, channel, body }) => {
    const message = { id: `${ME}:${++seq}`, channel, author: ME, body, sent_at_ms: Date.now() };
    (db.messages[channel] ||= []).push(message);
    listener?.({ payload: { type: "message", server, message } });
  },
  invite: () => null,
  kick: ({ server, peer }) => {
    srv(server).members = srv(server).members.filter((m) => m !== peer);
  },
  set_name: () => null,
  dial: () => null,
  voice_join: () => [],
  voice_leave: () => null,
  set_mute: () => null,
  voice_levels: () => ({ input_db: -60, transmitting: false, speaking: [] }),
  check_update: () => ({ version: "9.9.9", current: "0.1.1", notes: null }),
  install_update: () => null,
};

window.__TAURI__ = {
  core: {
    invoke: async (cmd, args = {}) => {
      calls.push({ cmd, args });
      if (!(cmd in handlers)) throw `mock: unexpected command ${cmd}`;
      return handlers[cmd](args);
    },
    Channel: class {
      constructor() {
        this.onmessage = null;
      }
    },
  },
  event: { listen: async (_name, cb) => (listener = cb) },
};

// ---- helpers ----

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const q = (sel) => document.querySelector(sel);

async function until(what, fn, timeout = 2000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    if (fn()) return;
    await sleep(20);
  }
  throw new Error(`timed out waiting for ${what}`);
}

function called(cmd) {
  return calls.filter((c) => c.cmd === cmd);
}

async function fillModal(values, button = ".modal button.primary") {
  await until("modal", () => q(".modal"));
  for (const [name, value] of Object.entries(values)) {
    const input = q(`.modal [name="${name}"]`);
    if (input.type === "checkbox") input.checked = value;
    else input.value = value;
  }
  q(button).click(); // a real click, which is how users submit
}

async function test(name, fn) {
  try {
    await fn();
    results.push({ name, ok: true });
  } catch (e) {
    results.push({ name, ok: false, error: String(e.message || e) });
    q("#modal-root").innerHTML = ""; // don't cascade into the next test
  }
}

// ---- flows ----

async function run() {
  await until("app boot", () => called("servers").length > 0 && q("#welcome-create"));

  await test("create server from welcome screen", async () => {
    q("#welcome-create").click();
    await fillModal({ name: "Test Den" });
    await until("create_server call", () => called("create_server").length === 1);
    await until("server header", () => q("#server-header").textContent.includes("Test Den"));
    await until("modal closed", () => !q(".modal"));
  });

  await test("create second server from the rail", async () => {
    q(".rail-btn.add").click();
    await fillModal({ name: "Second" });
    await until("second server", () => q("#server-header").textContent.includes("Second"));
  });

  await test("switch servers", async () => {
    [...document.querySelectorAll(".rail-btn")].find((b) => b.title === "Test Den").click();
    await until("header switch", () => q("#server-header").textContent.includes("Test Den"));
  });

  await test("create text channel", async () => {
    q('[data-new="false"]').click();
    await fillModal({ name: "memes" });
    await until("create_channel call", () => called("create_channel").some((c) => c.args.name === "memes" && !c.args.voice));
    await until("channel listed and selected", () => q(".channel.active")?.textContent.includes("memes"));
  });

  await test("empty channel name shows an error", async () => {
    const before = called("create_channel").length;
    q('[data-new="false"]').click();
    await fillModal({ name: "" });
    await until("error toast", () => [...document.querySelectorAll(".toast.error")].some((t) => t.textContent.includes("name")));
    if (called("create_channel").length !== before) throw new Error("created a channel with no name");
    q("[data-cancel]").click();
  });

  await test("send a message with Enter", async () => {
    const box = q("#composer");
    box.value = "hello hearth";
    box.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await until("send call", () => called("send").some((c) => c.args.body === "hello hearth"));
    await until("message rendered", () => q("#messages").textContent.includes("hello hearth"));
    if (box.value !== "") throw new Error("composer not cleared");
  });

  await test("invite a peer", async () => {
    q("#invite-btn").click();
    await fillModal({ peer: BOB });
    await until("invite call", () => called("invite").some((c) => c.args.peer === BOB));
    await until("modal closed", () => !q(".modal"));
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

  await test("join voice", async () => {
    q("[data-voice]").click();
    await until("voice_join call", () => called("voice_join").length === 1);
    await until("voice panel", () => !q("#voice-panel").hidden);
    q("#hangup").click();
    await until("voice_leave call", () => called("voice_leave").length === 1);
  });

  await test("update offer installs on click", async () => {
    await until("update toast", () => [...document.querySelectorAll(".toast")].some((t) => t.textContent.includes("9.9.9")));
    [...document.querySelectorAll(".toast")].find((t) => t.textContent.includes("9.9.9")).querySelector("button").click();
    await until("install_update call", () => called("install_update").length === 1);
  });

  await test("kick a member", async () => {
    q(`[data-kick="${BOB}"]`).click();
    await fillModal({});
    await until("kick call", () => called("kick").some((c) => c.args.peer === BOB));
  });

  if (errors.length) results.push({ name: "no uncaught errors", ok: false, error: errors.join("; ") });
  document.body.dataset.results = JSON.stringify(results);
}

window.addEventListener("load", () => run().catch((e) => {
  results.push({ name: "harness", ok: false, error: String(e) });
  document.body.dataset.results = JSON.stringify(results);
}));
