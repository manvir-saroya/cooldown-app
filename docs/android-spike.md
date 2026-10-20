# Android spike: results (2026-10-15)

Design spec section 10 asked whether Tauri 2 mobile can do three things. **All three pass on the Android 16 (API 36) arm64 emulator.** No real phone was used.

| Question | Result | Evidence |
|---|---|---|
| (a) Exact alarm that fires with the app closed | **Pass** | App sent home, process killed (`am kill`). The 1-minute test alarm fired 253 ms after its time and posted its notification. A bell set by the PC demo (synced, then scheduled as an exact alarm) fired at its reset time, 23:16:08, with "Your limit is back". |
| (b) Glance widget reading app-written JSON | **Pass** | Glance 1.1.1 builds against the Tauri-generated project, which now uses **Kotlin 2.2.10** (the spec feared 1.9.25). The widget was pinned with `requestPinAppWidget` and showed the synced window: "claude session_5h: 100% · resets 23:16, as of 23:14". |
| (c) LAN pairing + Noise handshake + sync with the PC | **Pass** | `PhoneClient` (sync crate built with `default-features = false`) paired with `pc_demo`: code shown, PC confirmed, sync received (1 window, 1 alert). After an app restart the pairing persisted and **Reconnect** (Noise IK to the cached address) got sync rev 2. |

So the plan stands: **Tauri 2 mobile, same Rust sync crate, small Kotlin pieces.** The native Kotlin fallback is not needed.

## What was built

- `apps/android/`: Tauri 2 app `app.cooldown.phone`. `src-tauri/src/phone.rs` is the phone side of pairing and sync; `lib.rs` has the commands and the bridge to Kotlin; `src/main.tsx` is a deliberately plain test screen (the real app will use `@cooldown/ui`).
- Kotlin, in `src-tauri/gen/android/app/src/main/java/app/cooldown/phone/`:
  - `NativePlugin.kt`: `scheduleAlarm` (exact, falls back to inexact without the permission; one alarm per id, replaced not stacked), `permissions`, `askPermissions`, `pinWidget`, `writeWidgetData`.
  - `AlarmReceiver.kt`: posts the notification.
  - `CooldownWidget.kt`: the Glance widget, reading `files/snapshots.json`.
- `crates/cooldown-sync/examples/pc_demo.rs` and `phone_demo.rs`: headless PC and phone for testing either side without the other app (in-memory keys, demo only: `pc_demo` auto-confirms pairings).

## How to reproduce

```sh
export JAVA_HOME=/opt/homebrew/opt/openjdk@17 ANDROID_HOME=$HOME/Library/Android/sdk \
       NDK_HOME=$HOME/Library/Android/sdk/ndk/30.0.16248370 PATH="$HOME/.cargo/bin:$PATH"
$ANDROID_HOME/emulator/emulator -avd resetbell36 -no-window -no-audio &      # headless
(cd apps/android && npx tauri android build --debug --apk --target aarch64)
$ANDROID_HOME/platform-tools/adb install -r apps/android/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
# PC side, on loopback, advertising the emulator's alias for the host:
cargo run -p cooldown-sync --example pc_demo -- 127.0.0.1 /tmp/qr.txt 10.0.2.2
```

Paste `/tmp/qr.txt` into the app (or `adb shell input text "$(cat /tmp/qr.txt)"`) and press Pair.

## Findings to carry into the real app

- **macOS firewall:** with the Mac's firewall on, an unsigned binary listening on the LAN address had its connections silently held (the kernel accepted them, the app never saw them); loopback is exempt. Hence `pc_demo` on 127.0.0.1 + `10.0.2.2`. The Windows app will meet the same idea as a Windows Firewall prompt on first pairing.
- **Permissions:** on API 36 with `targetSdk 37`, `SCHEDULE_EXACT_ALARM` starts denied and `POST_NOTIFICATIONS` needs a runtime prompt. In the spike they were granted with `adb` (`appops set … SCHEDULE_EXACT_ALARM allow`, `pm grant … POST_NOTIFICATIONS`); the app's **Grant permissions** button does the real flow. Without the exact permission alarms are inexact and may be late: the UI must say so (spec section 7).
- **`ACCESS_LOCAL_NETWORK`:** the restriction (`RESTRICT_LOCAL_NETWORK`) is off on this API 36 image, so it was not exercised. Android 17 devices will need the permission and its prompt before pairing.
- The Tauri `Plugin` base class already has `requestPermissions`; a plugin command cannot reuse that name.

## Background sync, widget choice, revocation (2026-10-19)

- **Background sync** (`SyncWorker.kt`, `background.rs`, `jni_entry.rs`). A WorkManager periodic job (15 minutes, network required, exponential backoff from 1 minute, first run 15 minutes after it is scheduled) runs while a PC is paired; the app schedules or cancels it on start, after pairing and after unpairing. Tauri cannot run its Rust side without an Activity, so the worker loads the library itself and calls one JNI function, `SyncBridge.runSyncRound(dataDir, key)`: Kotlin unwraps the phone key with the Keystore (the key never goes through the WebView), Rust opens the stores, reconnects (last address, then a multicast probe under a Wi-Fi multicast lock), takes the PC's newest sync, saves it, disconnects, and returns `{status, alarms, widget}` as JSON. Kotlin then re-sets the exact alarms and rewrites the widget file. One owner at a time (`Registry`): a round never opens the phone's files while the app has them; if the app runs in the same process the round reuses its phone and leaves its connection up, and a starting app waits for a round. An unreachable PC gets one retry after the backoff, then waits for the next period; with no PC paired the worker cancels itself.
- **Widget** (`CooldownWidget.kt`, `WidgetConfigActivity.kt`, `widget.rs`). Glance, scrolls (`LazyColumn`), every window in the app's order by default (Rust sorts like `sortSnapshots`), "as of" is when the PC's data arrived. The choice (snapshot ids, or none = every window including future ones) is one setting for all Cooldown widgets, kept in SharedPreferences. It is set on the configuration screen the launcher opens when a widget is added from the widget list, or in Settings. A widget pinned from Settings skipped the configuration screen on the Pixel launcher of the emulator; it shows every window until changed in Settings.
- **Revocation / unpair.** `Phone::unpair` and `Phone::pc_revoked` forget the PC's addresses and saved sync, `Phone::latest()` ignores any PC that is no longer paired, and the app cancels the alarms, blanks the widget and drops the job.
- **Testing the job on the emulator.** `adb shell cmd jobscheduler run -f -n androidx.work.systemjobscheduler app.cooldown.phone <job id>` is ignored by WorkManager while the periodic work is not due ("executed before schedule"). Make it due first: `adb shell "run-as app.cooldown.phone sqlite3 no_backup/androidx.work.workdb \"update WorkSpec set last_enqueue_time=1000 where worker_class_name like '%SyncWorker'\""`, then run the job. To close the app without a force stop (which cancels alarms and jobs), `adb shell "run-as app.cooldown.phone kill -9 <pid>"`; `am kill` does nothing while the widget host is bound.
- Not provable on an emulator: how OEM battery managers treat the job and the alarms, multicast discovery between real devices, a launcher that always opens the configuration screen when pinning.

## Not done (the real Android app)

- (Done 2026-10-16: the phone key is wrapped by the Android Keystore, a boot receiver restores alarms. App data is excluded from backup and device transfer.)
- The real UI and QR **camera** scanning exist (2026-10-16); the widget refresh budget (Glance redraws are cheap, but a launcher may throttle them) is untested.
- A real phone: OEM battery managers and real LAN multicast cannot be proven on an emulator.
