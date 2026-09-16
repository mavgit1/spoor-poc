# Spoor desktop app

Native curation UI for [Spoor](../PLAN.md). The Tauri backend calls the `spoor` library over **IPC** — it does not start the headless HTTP server.

## Dev

From this directory:

```bash
npm install
cargo tauri dev
```

If the Tauri CLI is not on `PATH`, the same command via npm is `npm run tauri dev`.

The first launch may download Chromium if no local Chrome is found (`SPOOR_CHROME` overrides). Optional `OPENROUTER_API_KEY` in a `.env` at the repo root is used only for LLM classify of ambiguous traffic.

`cargo tauri dev` starts Vite on port 1420 for hot reload. That is **not** the old Spoor HTTP API — the desktop process does not bind an API port. Production builds (`cargo tauri build`) embed the frontend and listen on nothing.

## Build

```bash
npm install
cargo tauri build
```

On macOS this produces `Spoor.app` and a `.dmg` under `target/release/bundle/` (workspace target directory at the repo root).

## Workflow

Start recording (window or tray) → browse in the recording Chrome → Stop → select API surfaces → Generate. Generate opens a native save dialog for `spoor-export.zip`. Closing the window hides Spoor to the tray; Quit from the tray to exit.

## Headless HTTP (not this app)

`spoor --serve [--token TOKEN]` in the `spoor` crate still binds a localhost API for agents. Every request requires `Authorization: Bearer`. The desktop app never starts that listener.
