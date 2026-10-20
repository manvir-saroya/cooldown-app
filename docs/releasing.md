# Releasing

A release is a version tag. Pushing `v0.1.0` starts the **Release** workflow
([`.github/workflows/release.yml`](../.github/workflows/release.yml)), which builds the Windows
installer and the Android APK and puts them in a **draft** GitHub release. A person reads the draft
and presses Publish; nothing publishes itself.

## What a release contains

| File | Notes |
|---|---|
| `Cooldown_<version>_x64-setup.exe` | Windows NSIS installer (per user, unsigned: SmartScreen warns) |
| `Cooldown_<version>_android.apk` | Android app, signed with the release key |
| `Cooldown_<version>_android-debug-signed.apk` | Same, but only when the signing secrets are missing (see below) |
| `SHA256SUMS.txt` | SHA-256 of every file above, for `sha256sum -c` / `Get-FileHash` |

The release notes come from [`.github/release-notes.template.md`](../.github/release-notes.template.md):
what Cooldown is, how to install on Windows (including SmartScreen: **More info**, then **Run
anyway**), how to sideload the APK, and the checksums. Edit the template to change the wording.

## Cutting a release

1. Pick the version and set it everywhere it is declared: `[workspace.package] version` in
   `Cargo.toml`, `version` in the root `package.json`, `apps/*/package.json`,
   `packages/*/package.json`, and both `tauri.conf.json` files. Then refresh the lockfiles
   (`cargo metadata > /dev/null`, `npm install --package-lock-only`).
   `node .github/scripts/check-version.mjs` checks that they all agree.
2. Merge to `main` and wait for the **Windows** and **Android** workflows to pass. The Windows one
   includes the smoke test that installs the app and starts it.
3. Tag that commit and push the tag:
   ```sh
   git tag v0.1.0
   git push origin v0.1.0
   ```
   The tag must be `v` plus the version from step 1, or the workflow stops at its first job.
4. Watch **Actions > Release**. It runs: check versions, build the installer, smoke test it, build
   the APK, then package (checksums, notes) and create the draft. About 25 minutes.
5. Open **Releases**, read the draft (notes, the four files), and test the installer and the APK
   if you can. Press **Publish release**. Tick **pre-release** for a release candidate (a version
   such as `0.2.0-rc1` is marked as one automatically).

If the workflow fails, fix the cause, delete the tag (`git push origin :refs/tags/v0.1.0`), and tag
again. Re-running the workflow for the same tag replaces its own earlier draft. It refuses to touch
a release that has already been published.

## Dry run

**Actions > Release > Run workflow** (pick a branch) builds and packages everything exactly as a
tag would, but creates no release. The result is the `release-bundle` artifact (the files and
`release-notes.md`) and a job summary showing the rendered notes. Use it to check a change to the
release process.

## Android signing

Android only installs, and later updates, an APK that is signed, and an update must be signed with
the same key as the app it replaces. Releases are signed with a key you keep; **if you lose the key,
installed phones can never update and everyone has to uninstall first.** Back up the keystore and
its passwords somewhere other than GitHub.

### One-time setup

Create the keystore (needs a JDK; keep the file out of the repository, `*.keystore` and `*.jks`
belong in a password manager or encrypted backup):

```sh
keytool -genkeypair -v -keystore cooldown-release.keystore -alias cooldown \
  -keyalg RSA -keysize 4096 -validity 10000
```

Then add four **repository secrets** (Settings > Secrets and variables > Actions), or with the
`gh` CLI:

| Secret | Value |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | the keystore file, base64-encoded in one line |
| `ANDROID_KEYSTORE_PASSWORD` | the keystore password |
| `ANDROID_KEY_ALIAS` | the alias (`cooldown` above) |
| `ANDROID_KEY_PASSWORD` | the key password (the same as the keystore password if you pressed Return) |

```sh
base64 -i cooldown-release.keystore | tr -d '\n' | gh secret set ANDROID_KEYSTORE_BASE64
gh secret set ANDROID_KEYSTORE_PASSWORD
gh secret set ANDROID_KEY_ALIAS
gh secret set ANDROID_KEY_PASSWORD
```

### How it behaves

- **All four secrets set:** the workflow writes the keystore to a temporary file, builds a release
  (R8-minified, not debuggable) APK that Gradle signs with it, checks with `apksigner` that the
  signature is valid and is not the debug key, and names the file `Cooldown_<version>_android.apk`.
  The release notes include the certificate fingerprint.
- **None set:** the APK is built the same way but signed with the throwaway Android debug key and
  named `Cooldown_<version>_android-debug-signed.apk`. It installs, but the key is generated afresh
  for every build, so Android refuses to update one such APK with the next. The release notes say
  so. Fine for a first look, not for real use.
- **Only some set:** the workflow fails on purpose, so a half-configured release is never
  published as debug-signed by mistake.

The secrets reach only the two steps that need them (restoring the keystore and the Gradle build),
and the workflow only runs for tags and manual runs, never for pull requests. The signing setup is in
`apps/android/src-tauri/gen/android/app/build.gradle.kts`: it reads `ANDROID_KEYSTORE_PATH`,
`ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS` and `ANDROID_KEY_PASSWORD` from the environment,
so a local signed build works the same way:

```sh
export ANDROID_KEYSTORE_PATH=/path/to/cooldown-release.keystore ANDROID_KEYSTORE_PASSWORD=... \
       ANDROID_KEY_ALIAS=cooldown ANDROID_KEY_PASSWORD=...
(cd apps/android && npx tauri android build --apk --target aarch64)
```

## Windows smoke test

[`.github/scripts/windows-smoke-test.ps1`](../.github/scripts/windows-smoke-test.ps1) runs after the
installer is built, in `windows.yml` (every push to `main`) and in `release.yml` (before anything is
packaged). It installs the installer silently (`/S`), starts the installed app with `--hidden`,
waits 20 seconds, and requires that `%LOCALAPPDATA%\app.cooldown.desktop\logs\cooldown.log` exists,
says `Cooldown <version> starting`, and contains neither `panic` nor `startup failed`. It then stops
the app and uninstalls it silently, and fails if the program files are still there.

## Not done yet

- The Windows installer is not code-signed, so SmartScreen warns on every download. A certificate
  costs money; until then the release notes tell people to click **More info**, then **Run anyway**.
- The APK is not published to Google Play or F-Droid; it is sideloaded from the Releases page.
- The workflow does not check that the tagged commit passed the test workflows. Tag a commit on
  `main` that is green.
