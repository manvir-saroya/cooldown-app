# Cooldown desktop

The Windows app (it also runs on macOS for development): a tray app around the shared `@cooldown/ui`.

- `src/TauriSnapshotSource.ts`: the UI's data seam, backed by Tauri commands and the `cooldown://changed` event.
- `src-tauri/src/backend.rs`: the engine. Polls the adapters through `cooldown-core`'s `Registry`, stores readings in SQLite, owns reset alerts (its own 15-second timer and the core `Reconciler`, because Tauri cannot schedule notifications on desktop) and pushes state to paired phones.
- `src-tauri/src/phone.rs`: the `cooldown-sync` PC server. It starts only when you open "Pair a device" or a phone is already paired.
- `src-tauri/src/hook.rs`: installs the Claude Code statusline hook, shipped next to the app as the `cooldown-statusline` sidecar, but only when you press Install.

Closing the window hides it. Bells keep ringing from the tray; quit from the tray menu.

## Run and test

```sh
export PATH="$HOME/.cargo/bin:$PATH"
npm run tauri dev -w @cooldown/desktop        # app with hot reload
npm test -w @cooldown/desktop                 # TypeScript tests
node apps/desktop/scripts/build-sidecar.mjs --debug && cargo test -p cooldown-desktop
cd apps/desktop && npx tauri build --bundles nsis   # installer (on Windows; CI does this)
```

`npm run dev:web` opened in a normal browser has no backend, so it shows the labelled sample data.
