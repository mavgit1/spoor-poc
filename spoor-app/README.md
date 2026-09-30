# Spoor desktop app

A tray app and small site manager for [Spoor](../README.md). It lists your
sites with **Open** (log in), **Record**, **Check** (run the site's check
script) and **Close browser**, plus recent recordings and a *Stop all sites*
kill switch.

The app does not drive browsers itself. On launch it attaches to a running
`spoor serve`, or runs the service in-process, and every button calls the
same local API the CLI and integrations use. While the app is open, `spoor`
commands in a terminal use its service.

## Dev

From this directory:

```bash
npm install
npm run tauri dev
```

The first launch downloads the pinned Chromium if it isn't cached yet.

## Build

```bash
npm install
npm run tauri build
```

Output goes to `target/release/bundle/` at the repo root.

Closing the window hides it to the tray. Use **Quit** in the tray menu to
exit; it closes every site browser and stops the service.
