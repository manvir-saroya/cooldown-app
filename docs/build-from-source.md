# Build Cooldown from source

Use this guide if you want to run the tests, try a change, or build the Windows installer or the Android app yourself. If you only want to use Cooldown, download it from the [Releases page](../../../releases) instead: see [Install on Windows](install-windows.md) and [Install on Android](install-android.md).

Before you send a change, read [CONTRIBUTING.md](../CONTRIBUTING.md).

**Contents:** [What you need](#1-what-you-need) · [Get the code](#2-get-the-code) · [Run the tests](#3-run-the-tests) · [Run the app](#4-run-the-desktop-app-while-you-work-on-it) · [The sidecar](#the-sidecar-step-explained) · [Windows installer](#5-build-the-windows-installer) · [Android app](#6-build-the-android-app) · [Problems](#when-a-build-fails)

## 1. What you need

### For everything

| Tool | Version | How to get it |
|---|---|---|
| **Git** | any recent | [git-scm.com](https://git-scm.com) |
| **Node.js** (comes with **npm**) | **24** (what CI uses) or newer | [nodejs.org](https://nodejs.org) |
| **Rust**, installed with **rustup** | stable, **1.88 or newer** | [rustup.rs](https://rustup.rs) |

The repository contains a `rust-toolchain.toml`, so rustup picks the stable toolchain with `rustfmt` and `clippy` for you. Check your tools:

```sh
git --version
node --version     # v24 or higher
npm --version
rustc --version    # 1.88 or higher
cargo --version
```

### For the desktop app (Tauri 2)

Cooldown's desktop and Android apps are built with [Tauri 2](https://v2.tauri.app). Tauri needs some system tools. The official list for every system is here: [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/). In short:

**Windows 10 or 11 (this is the supported system for the desktop app)**

1. **Microsoft C++ Build Tools.** Install *Visual Studio Build Tools* from [visualstudio.microsoft.com/downloads](https://visualstudio.microsoft.com/downloads/) (scroll to *Tools for Visual Studio*), and in the installer tick the workload **Desktop development with C++**.
2. **WebView2.** Windows 11 and recent Windows 10 already have it. If not, install the *Evergreen Bootstrapper* from Microsoft.
3. **Rust with the MSVC toolchain.** When rustup asks, keep the default (`x86_64-pc-windows-msvc`). If you installed another one: `rustup default stable-msvc`.
4. Open a new terminal after installing, so the tools are on your `PATH`.

**macOS** (works for development, but is not a supported release)

- Xcode Command Line Tools: `xcode-select --install`.

**Linux** (the shared code and its tests work, the desktop app itself is not supported)

- The packages listed in the Tauri prerequisites for your distribution (WebKitGTK and friends). You need them even just to compile the whole workspace. <!-- TODO: verify on Linux -->

### For the Android app (optional)

You only need this to build the APK.

| Tool | Version used | Notes |
|---|---|---|
| **JDK** | **17** | For example Temurin 17. |
| **Android SDK** | platform **37** (the app compiles and targets API 37), build-tools 36.x | Install with Android Studio's SDK Manager, or the command-line `sdkmanager`. |
| **Android NDK** | **30.0.16248370** (the version used on the maintainer's machine; CI uses the one on its runner) | Needed to compile the Rust code for Android. |
| **Rust target** | `aarch64-linux-android` | `rustup target add aarch64-linux-android` |

The Gradle project sets `compileSdk = 37` and `targetSdk = 37`, so it needs the **Android 37 platform** (`platforms;android-37`) installed; `minSdk` is 24 (Android 7.0). An older platform such as 36 is not enough on its own to build. (The emulator the maintainer tests on runs an API 36 system image. That is the phone being simulated, not the build platform.)

You also need three environment variables. Examples (change the paths to yours):

```sh
# macOS or Linux (bash or zsh)
export JAVA_HOME=/path/to/jdk-17
export ANDROID_HOME=$HOME/Library/Android/sdk          # Linux: usually $HOME/Android/Sdk
export NDK_HOME=$ANDROID_HOME/ndk/30.0.16248370
```

```powershell
# Windows PowerShell
$env:JAVA_HOME = "C:\Program Files\Eclipse Adoptium\jdk-17"
$env:ANDROID_HOME = "$env:LOCALAPPDATA\Android\Sdk"
$env:NDK_HOME = "$env:ANDROID_HOME\ndk\30.0.16248370"
```

The project's Gradle setup downloads its own Gradle (version 9.6.1) and the Android libraries the first time you build. That needs an internet connection and some patience.

## 2. Get the code

```sh
git clone <this repository URL>
cd cooldown
npm ci
```

`npm ci` installs the exact versions in `package-lock.json`. (`npm install` also works.) Rust libraries are downloaded automatically the first time you run `cargo`.

### What is where

```
crates/
  cooldown-core/        data types, adapter registry, SQLite store, alert logic
  cooldown-statusline/  the Claude Code statusline hook (a small program)
  cooldown-adapters/    the Claude, Codex, OpenRouter, DeepSeek and xAI sources
  cooldown-sync/        QR pairing and the encrypted PC-to-phone link
packages/
  snapshot-schema/      the data contract: types, validators, JSON Schema, test fixtures
  ui/                   the React screens shared by both apps
apps/
  desktop/              Tauri 2 Windows app
  android/              Tauri 2 Android app
docs/                   documentation
```

## 3. Run the tests

From the repository root:

```sh
# 1. TypeScript: type check and tests for every package
npm run typecheck
npm test

# 2. The desktop crate needs the hook program to exist first (see "The sidecar step")
node apps/desktop/scripts/build-sidecar.mjs --debug

# 3. Rust: tests, lints and formatting
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

All of these must pass before you open a pull request. They are the same checks that CI runs.

Handy shortcuts while you work:

```sh
npm test -w @cooldown/snapshot-schema      # one TypeScript package
npm test -w @cooldown/ui
cargo test -p cooldown-adapters            # one Rust crate
cargo fmt --all                            # fix formatting
```

If you change the data contract, regenerate the JSON Schema with `npm run build:schema -w @cooldown/snapshot-schema`. See [the data-contract rule](../CONTRIBUTING.md#the-data-contract-rule).

No test uses the real network, your real Claude or Codex, or your real keys.

## 4. Run the desktop app while you work on it

```sh
npm run tauri dev -w @cooldown/desktop
```

This builds the hook program, starts the screens on `http://127.0.0.1:1420` and opens the Cooldown window. It reloads when you change the TypeScript code. The first run compiles a lot of Rust and takes a while.

> **Careful:** a development run uses the **same data folder, credential store entries and Claude settings** as an installed Cooldown. Pressing **Install hook** in a dev build edits your real `~/.claude/settings.json` (after a backup), just like the installed app. Quit the installed copy before you run a dev copy, and uninstall the hook when you are done testing.

Just want to look at the screens with made-up data, with no backend?

```sh
npm run dev -w @cooldown/ui          # the shared screens with sample data
```

The screens are then served on `127.0.0.1`. Open the address it prints in your browser. The data is labelled as sample data.

Other places the app writes (for reference): [Where your data and logs live](install-windows.md#where-your-data-and-logs-live).

### The sidecar step explained

Cooldown ships a small helper program, `cooldown-statusline`, next to the app. Claude Code runs it to hand over your limits. Tauri calls such a helper a *sidecar*.

Tauri expects the sidecar to exist, with the name of your computer's target at the end, **before** it compiles the desktop app:

```
apps/desktop/src-tauri/binaries/cooldown-statusline-<target-triple>[.exe]
```

This script builds it and puts it in the right place:

```sh
node apps/desktop/scripts/build-sidecar.mjs --debug     # fast; for tests and dev runs
node apps/desktop/scripts/build-sidecar.mjs             # optimized; for releases
```

You only need to run it by hand before `cargo test` or `cargo clippy` on the whole workspace. `npm run tauri dev` and `npx tauri build` run it for you. It needs `cargo` and `rustc` on your `PATH`. To build for another target, set `TAURI_TARGET_TRIPLE` (for example `x86_64-pc-windows-msvc`).

## 5. Build the Windows installer

Do this on Windows. (CI does the same on a Windows machine for every release.)

```powershell
cd apps\desktop
npx tauri build --bundles nsis
```

This builds the hook program, the screens and the app, and then packs the installer with NSIS. The first time, Tauri downloads the NSIS tools it needs, so you need an internet connection.

The installer is created at:

```
target\release\bundle\nsis\Cooldown_<version>_x64-setup.exe
```

(The `target` folder is at the **repository root**, not inside `apps\desktop`.) `<version>` comes from `apps/desktop/src-tauri/tauri.conf.json`.

The installer is **not code-signed**, so Windows SmartScreen will warn when you run it. See [Install on Windows](install-windows.md#3-install).

On macOS `npx tauri build` makes an `.app` and a `.dmg` for development tests, not for sharing.

## 6. Build the Android app

You need the Android tools from [section 1](#for-the-android-app-optional). The Android project is already in the repository (`apps/android/src-tauri/gen/android`), so you do not need to run `tauri android init`.

```sh
cd apps/android
npx tauri android build --debug --apk --target aarch64
```

The APK is created at:

```
apps/android/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```

This is a **debug** build, signed with a throwaway debug key. It is fine for your own phone. It is for 64-bit ARM (arm64) phones. Releases for other people are built by the project's GitHub Actions release workflow, as an optimized (not debug) build signed with the project's own release key, which is kept as a secret and is not in this repository.

Install it on a phone with USB debugging turned on:

```sh
$ANDROID_HOME/platform-tools/adb install -r apps/android/src-tauri/gen/android/app/build/outputs/apk/universal/debug/app-universal-debug.apk
```

Or copy the file to the phone and open it, as in [Install on Android](install-android.md#2-install-it-sideloading).

To use an Android emulator instead of a phone, make an arm64 virtual device. Notes from the maintainers' tests, including how an emulator reaches the PC, are in [android-spike.md](android-spike.md). An emulator cannot show real battery-saver behavior, or find the PC with network multicast.

Building on Linux is covered by CI and building on macOS is what the maintainer uses.

**On Windows** the build works, with two differences:

- Tauri links the compiled Rust library into the Android project with a symbolic link, and Windows only allows those with **Developer Mode** on (Settings > System > For developers). Without it the build stops with "Creation symbolic link is not allowed for this system". If you would rather not turn it on, build the Rust part first, copy the library in yourself and let Gradle skip its Rust step:

  ```powershell
  cd apps\android
  npx tauri android build --debug --apk --target x86_64   # compiles Rust, then stops at the link step
  cd src-tauri\gen\android
  New-Item -ItemType Directory -Force app\src\main\jniLibs\x86_64
  Copy-Item ..\..\..\..\..\target\x86_64-linux-android\debug\libcooldown_android_lib.so app\src\main\jniLibs\x86_64\
  .\gradlew.bat assembleX86_64Debug -x rustBuildX86_64Debug
  ```

  The APK is then at `app/build/outputs/apk/x86_64/debug/app-x86_64-debug.apk`.
- An Intel or AMD PC runs **x86_64** emulator images, not arm64, so build with `--target x86_64` (`rustup target add x86_64-linux-android`) and create the virtual device from `system-images;android-36;google_apis;x86_64`. The emulator needs the Windows Hypervisor Platform; `emulator -accel-check` tells you whether it is usable.

## When a build fails

| Message or problem | What to try |
|---|---|
| `linker 'link.exe' not found` or `link.exe` errors (Windows) | Install **Desktop development with C++** from the Visual Studio Build Tools, then open a **new** terminal. |
| A message about a missing `cooldown-statusline` sidecar, or `binaries/cooldown-statusline-...` not found | Run `node apps/desktop/scripts/build-sidecar.mjs --debug`. |
| `command not found: cargo` or `rustc` | Open a new terminal after installing rustup, or run `. "$HOME/.cargo/env"`. |
| Rust says your compiler is too old | `rustup update stable`. |
| `JAVA_HOME` is not set, or Gradle says it needs a newer Java | Install JDK 17 and set `JAVA_HOME`. |
| `NDK_HOME` or "NDK not found" | Install the NDK and set `NDK_HOME` to its folder. |
| Android: "failed to find tool aarch64-linux-android..." | Check `NDK_HOME`, and run `rustup target add aarch64-linux-android`. |
| Android: Gradle asks for a missing platform or licence | Install the SDK platform in the SDK Manager and accept the licences (`sdkmanager --licenses`). |
| Android on Windows: "Creation symbolic link is not allowed for this system" | Turn on Developer Mode, or use the copy-and-Gradle steps in [section 6](#6-build-the-android-app). |
| Port `1420` is in use | Another dev copy is running. Close it. |
| Two cargo commands wait on each other | Give each its own target folder: set `CARGO_TARGET_DIR` to a different folder for each. |
| A test fails only on your machine | Open an issue with the output. If you changed fixtures, see [the data-contract rule](../CONTRIBUTING.md#the-data-contract-rule). |

Still stuck? Open an [issue](../../../issues/new/choose) and include your operating system, `node --version`, `rustc --version` and the last lines of the error.
