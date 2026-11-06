# Rust Beacon Builder Plugin

This trusted server-only plugin registers the full Rust beacon as an agent build
provider. Select **Rust Beacon** from the Builder's **Agent Type** field, choose
targets, and start the build normally. Output, history, downloads, expiration,
rate limits, RBAC, and artifact hooks remain owned by the Overlord server.

The resulting client implements the full version 1 wire contract: enrollment and
Ed25519 challenge signing, TLS identity pinning, heartbeat/reconnect, console,
files, processes, screenshot, desktop streaming and input, clipboard, keylogger,
wscript/cmd/powershell execution, persistence, elevation, update, proxy tunnels,
native plugins, WinRE, webcam, audio, and WebRTC signaling.

During compilation the Build page shows estimated overall/target percentages,
completed Cargo crates, elapsed time, the current compile or link/LTO phase,
and a rolling ETA. Completed target durations are retained in plugin data to
improve later estimates, and the persistent Cargo target directory makes
subsequent builds substantially faster.

The **Cargo Jobs** setting defaults to `0`, which leaves parallelism at Cargo's
host-CPU default. Set a positive number only when the server needs an explicit
resource cap.

The plugin locates `Overlord-Beacon` beside the server runtime or under its
`dist/` directory. Production package scripts copy that source automatically.

Builder releases compile in the selected server URL, agent token, signed build
tag, and the server's active TLS SPKI pins. Runtime `OVERLORD_SERVER`,
`OVERLORD_AGENT_TOKEN`, and `OVERLORD_BUILD_TAG` variables can still override
those defaults.

All provider builds use Cargo's `--release` profile: `opt-level = 3`, full LTO,
one codegen unit, symbol stripping, and `panic = "abort"`. The Windows binary is
a GUI-subsystem executable and detaches from any inherited console at startup.
