<p align="center">
  <img src="./assets/logo.png" width="120" alt="Playmate logo" />
</p>

<h1 align="center">Playmate</h1>

<p align="center">
  LAN co-op NES/Famicom emulator — pair two computers, take P1 and P2, and play together.
</p>

<p align="center">
  <a href="https://github.com/zlx2019/playmate/actions/workflows/ci.yml"><img src="https://github.com/zlx2019/playmate/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/zlx2019/playmate/releases"><img src="https://img.shields.io/github/v/release/zlx2019/playmate?include_prereleases" alt="Release" /></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-e60012" alt="Platform" />
</p>

<p align="center">
  <b>English</b> · <a href="./README.zh-CN.md">简体中文</a>
</p>

---

Playmate turns two computers on the same LAN into one two-player Famicom. One side creates a room; the other discovers it automatically and joins with a 4-digit PIN. Only the host runs the emulator and needs the ROM — video and audio are streamed to the guest, and the guest's button presses stream back. Swap seats any time; either machine can be P1.

## ✨ Features

- 🎮 **Solo & couch co-op** — one machine, one or two players; the keyboard ships with a two-player layout out of the box.
- 🌐 **LAN netplay** — rooms are discovered automatically via mDNS and joined with a 4-digit PIN; no IP addresses to type.
- 🖥️ **Host-authoritative** — only the host emulates and owns the ROM; frames travel as XOR-delta + lz4 streams, with input lag of 1–2 frames on a typical LAN.
- 🔁 **Auto-reconnect** — network drops retry with exponential backoff and put you straight back into the running game.
- 🕹️ **Plug-and-play gamepads** — controllers take P1 / P2 in the order they connect, zero configuration.
- ⌨️ **Rebindable keys** — remap everything in Settings, numpad and modifier keys included; saved to `playmate.toml`.
- 🎨 **Famicom-flavoured UI** — a dark retro theme in the classic red and white.

## 📥 Install

Grab a build from [Releases](https://github.com/zlx2019/playmate/releases). Every artifact has a `.sha256` checksum next to it.

| Platform | Artifact | Notes |
|---|---|---|
| macOS (Apple Silicon) | `Playmate-vX.Y.Z-aarch64-apple-darwin.dmg` | drag Playmate into Applications |
| macOS (Intel) | `Playmate-vX.Y.Z-x86_64-apple-darwin.dmg` | drag Playmate into Applications |
| Windows | `Playmate-vX.Y.Z-x86_64-pc-windows-msvc.zip` | unzip and run `Playmate.exe` |
| Linux | `Playmate-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` | untar and run `Playmate`; a `playmate.desktop` and icon are included for desktop integration |

> Builds are currently unsigned. On macOS, right-click the app and choose **Open** the first time (or run `xattr -cr /Applications/Playmate.app`). Windows SmartScreen may ask for confirmation as well.

## 🕹️ ROMs

Playmate ships no games — bring your own legally obtained `.nes` files. Put them into a `roms/` folder in either location; both are scanned and merged, deduplicated by file name:

- next to the program (or next to `Playmate.app`) — portable style;
- the user data directory: `~/Library/Application Support/Playmate` (macOS), `%APPDATA%\Playmate` (Windows), `~/.config/playmate` (Linux).

The game list page has **Open ROM folder** and **Refresh** buttons, so there is no path to remember. In netplay only the host needs the ROM file.

## 🎮 Default controls

| Famicom | P1 | P2 |
|---|---|---|
| D-pad | W / A / S / D | ↑ / ↓ / ← / → |
| B | J | Numpad 0 |
| A | K | Numpad . |
| Select | Left Shift | — (the real 2P controller had none) |
| Start | Enter | Numpad Enter |

Everything is rebindable in Settings; `Esc` is reserved for leaving a game. For hand-editing the config, see [playmate.example.toml](./playmate.example.toml).

## 🌐 Netplay in four steps

1. One side opens **LAN play** and creates a room, with a chosen or auto-generated 4-digit PIN.
2. On the same LAN the room shows up on the other machine by itself — select it and enter the PIN.
3. Inside the room, swap P1 / P2 freely; the host picks a game and starts.
4. Quitting a game returns both sides to the room, ready for the next round.

## 🔨 Build from source

```bash
git clone https://github.com/zlx2019/playmate.git
cd playmate
cargo run --release
```

The Rust version is pinned by `rust-toolchain.toml` and installed automatically by `rustup`. On Linux, audio and gamepad support need extra headers first:

```bash
sudo apt-get install -y libasound2-dev libudev-dev pkg-config
```

The workspace splits into `apps/playmate-app` (egui UI, audio, input, netplay tasks), `deps/playmate-core` (the emulator core, wrapping [tetanes-core](https://github.com/lukexor/tetanes)) and `deps/playmate-net` (wire protocol, frame compression, PIN handshake, mDNS discovery). Lints, tests, hooks and the release flow are covered in [CONTRIBUTING.md](./CONTRIBUTING.md).

## ❓ FAQ

**macOS says the app is damaged / from an unidentified developer.**
The build is not notarized yet. Right-click → Open once, or clear the quarantine flag with `xattr -cr /Applications/Playmate.app`.

**Rooms never show up on macOS.**
macOS 15+ asks for **Local Network** permission on first launch — it must be allowed, otherwise mDNS discovery fails silently. Re-enable it under System Settings → Privacy & Security → Local Network.

**Rooms never show up on Windows.**
Discovery needs the firewall to let `Playmate.exe` communicate on private networks — allow it when Windows asks on first run.

**Does the guest need the ROM file?**
No. Only the host loads the ROM; the guest receives the video/audio stream and sends button input back.

**Which keys do I use in netplay?**
Both the P1 and the P2 keyset control your own seat, so use whichever half of the keyboard is comfortable — handy on laptops without a numpad.

**In single player, the other keyset moves my character too.**
That is the game, not the emulator: many FC titles read both controllers OR-ed together in single-player mode.

## 📄 License

[MIT](./LICENSE) — Playmate is an emulator and contains no copyrighted game content.
