# Hearth

Peer-to-peer, end-to-end encrypted chat with servers, channels, voice, and
screen sharing. No accounts, no central server: your identity is a key pair
that never leaves your device.

## What works

- **Identity**: an ed25519 key is your account and your network address. Share
  your peer ID; friends dial you by it.
- **Servers and channels**: text and voice channels, invites, kicks. History
  syncs between members, so anyone online can bring you up to date.
- **Encryption**: messages are sealed (XChaCha20-Poly1305) under a server key
  only members hold. Removing someone rotates the key.
- **Voice**: Opus at 64 kbps with forward error correction over QUIC datagrams,
  with a jitter buffer and loss concealment. Mute and deafen. If there's no
  microphone you join listen-only.
- **Screen share** (desktop app): hardware H.264 up to 1440p60 through WebCodecs,
  sent only to people who click *Watch*, one QUIC stream per frame, and frames
  are skipped for viewers who fall behind.
- **Offline delivery**: run `hearth node` somewhere always-on. It stores and
  forwards servers' encrypted history but is never given keys.
- **Your own relay**: `--relay https://your.relay` to avoid public relays.

## Quick start

```sh
# CLI
cargo run --release -p hearth-cli -- chat --name you
#   /new My Server      create a server
#   /invite <peer-id>   invite a friend (they /accept)
#   /voice              join voice     /help for everything

# Desktop app
cargo install tauri-cli --version "^2" --locked
cd crates/hearth-desktop && cargo tauri dev
```

Set `HEARTH_DATA_DIR` (app) or `--data-dir` (CLI) to run several identities on
one machine.

### Windows

Download the installer from the latest GitHub Actions run or release. To build
it yourself you need [Rust](https://rustup.rs) and Visual Studio Build Tools
with *Desktop development with C++* (it includes CMake, which libopus needs).
WebView2 ships with Windows 10 and 11.

```powershell
cargo install tauri-cli --version "^2" --locked
cd crates\hearth-desktop
cargo tauri build   # installers land in target\release\bundle\
```

### Always-on node and relay

```sh
hearth node                       # prints its ID; owners add it with /host <id>
hearth node --allow <owner-id>    # only host servers for these owners
cargo install iroh-relay --features server   # optional self-hosted relay
```

## How it works

```
crates/
  hearth-core     identity, event log, crypto, sync engine, media transport
  hearth-media    microphone/speakers, Opus, jitter buffer, mixing
  hearth-cli      `hearth` command: interactive client and storage node
  hearth-desktop  Tauri app (Rust commands + plain HTML/JS UI in ui/)
```

**Transport.** [iroh](https://iroh.computer) QUIC connections addressed by
public key. It hole-punches direct connections and falls back to relays, which
see only TLS ciphertext.

**Servers are event logs.** Each author numbers their events per server, and
every event is signed. Peers sync by exchanging the highest number they hold per
author and sending what's missing. All peers apply the same deterministic rules,
so they converge on the same state whatever order events arrive in.

**Membership** is owner-signed plaintext (`Add`, `Remove`, `AddHost`,
`Rotate`), so a storage node can enforce it without reading anything else.
Every other event is sealed under the current epoch key. The owner publishes a
hash of each epoch key, so a key received from any member can be checked.

**Removal** rotates the key, and records the last event the owner accepts from
the removed member. Anything they post later is rejected, even if they backdate
it to before their removal.

**Media.** Each voice packet is sealed with a key derived from the epoch key and
the channel, and checked against a per-sender replay window. Packets are
accepted only from current members in your voice channel.

## Security model and current limits

Hearth protects message content from everyone who isn't a member, including
relays, storage nodes and network observers. Be aware of these limits:

- **Metadata.** Relays and nodes can see which peer IDs talk to each other, and
  when. Nodes can also see who belongs to a server.
- **Forward secrecy** is per epoch, not per message. Someone who steals a
  member's key can read that epoch's history. Moving group encryption to MLS is
  on the roadmap.
- **Keys at rest.** Keys are stored unencrypted in the data directory, so
  protect your device. OS keychain integration is planned.
- **Permissions.** Only the owner can invite, remove and create channels. There
  are no roles yet.
- **DMs** are live-only (both people online). Planned: two-person servers, which
  would add history and offline delivery.
- **Screen share** requires WebCodecs. It works in the Windows app (WebView2),
  but Linux WebKitGTK may not support it yet.
- **Mesh voice.** Every participant sends audio to every other participant,
  which works well for small groups. Routing large rooms through a node is
  planned.

## Development

```sh
cargo test --workspace
```

The CLI accepts `--tone <hz>` and `--record <file.wav>`, which make voice
testable without a microphone.
