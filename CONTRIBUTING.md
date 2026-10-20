# Contributing to Cooldown

Thank you for wanting to help. Cooldown is a free, open-source, non-commercial app. It is made by volunteers and it stays that way: no ads, no paid tier, no tracking.

This file is the rulebook. It is short on purpose. Please read the [project principles](#project-principles) before you write any code, because they are not up for negotiation.

By taking part you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md). Cooldown is [MIT licensed](LICENSE); when you contribute, you agree that your work is released under the same license.

## Contents

1. [Project principles](#project-principles)
2. [Ways to help](#ways-to-help)
3. [Set up your machine](#set-up-your-machine)
4. [Branch and pull request flow](#branch-and-pull-request-flow)
5. [Commit messages](#commit-messages)
6. [What must pass before you open a pull request](#what-must-pass-before-you-open-a-pull-request)
7. [The data-contract rule](#the-data-contract-rule)
8. [Adding a provider adapter](#adding-a-provider-adapter)
9. [Reusing other people's code](#reusing-other-peoples-code)
10. [Tests and test data](#tests-and-test-data)
11. [Changing the screens](#changing-the-screens)
12. [Writing documentation](#writing-documentation)
13. [Security-sensitive changes](#security-sensitive-changes)

## Project principles

These are hard rules. A pull request that breaks one will not be merged, however good the code is.

1. **Local-first.** Everything runs on the user's own machines. The PC and the phone talk to each other directly on the user's own network.
2. **No hosted service.** No Cooldown server, no account, no login, no relay run by the maintainers. Do not add one, and do not add a dependency that quietly needs one.
3. **Never touch subscription logins.** Cooldown must never read:
   - OAuth tokens for Claude or Codex,
   - credential files such as `~/.claude/.credentials.json` or `~/.codex/auth.json`,
   - browser cookies,
   - the operating system's credential store, except for Cooldown's own entries.

   It must never call private or undocumented usage endpoints, such as `/api/oauth/usage` or `wham/usage`. The only supported Claude source is Claude Code's own statusline hook. Codex is read through its own command line or its local session logs.
4. **Keys never leave the device.** API keys are typed on one device, stored in that device's credential store, sent only to the provider they belong to, and never synced, logged, put in a snapshot or placed in an error message.
5. **Only sanitized readings cross the network.** A paired phone gets percentages, balances, reset times and labels. Nothing else. The code that builds that payload uses an allowlist, not "serialize whatever we have".
6. **Every number shows its source and its age.** A figure is labelled official, estimated, scraped or manual, and it says when it was last observed. If a source fails, keep the last good value and mark it stale or degraded. Never guess and never blank the screen.
7. **No telemetry.** No analytics, no crash reporting, no "phone home", not even anonymous. Logs stay on the user's machine.
8. **Credit what you reuse.** If you reuse or adapt open-source code, say so in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). See [Reusing other people's code](#reusing-other-peoples-code).
9. **No monetization.** No donation prompts, paid features or affiliate links inside the app.

## Ways to help

- **Report a bug or ask for a feature.** Use the [issue forms](../../issues/new/choose). Search first in case someone already did.
- **Try it on your machine** and tell us what was confusing. Real-world reports from Windows and Android devices are the most valuable thing right now.
- **Improve the docs.** If a step did not work for you, fix the step.
- **Fix a bug or build a feature.** For anything bigger than a small fix, please open an issue first and say what you plan to do. That saves you from building something that cannot be merged.
- **Add a provider.** See [Adding a provider adapter](#adding-a-provider-adapter).
- **Found a security problem?** Do not open a public issue. Follow [SECURITY.md](SECURITY.md).

## Set up your machine

The full steps, for each operating system, are in [docs/build-from-source.md](docs/build-from-source.md). In short you need Node 24, a stable Rust toolchain installed with rustup, and the Tauri 2 prerequisites for your system. The Android app also needs JDK 17, the Android SDK and the NDK.

```sh
npm install
node apps/desktop/scripts/build-sidecar.mjs --debug   # the desktop crate needs this file to exist
npm test
cargo test --workspace
```

If those pass, you are ready.

## Branch and pull request flow

1. **Fork** the repository on GitHub and clone your fork.
2. **Branch from `main`.** Give the branch a short, clear name, such as `fix/codex-path-on-windows` or `feature/gemini-adapter`.
3. **Keep each branch to one change.** Small pull requests get reviewed faster than big ones. Do not mix a refactor with a bug fix.
4. **Commit as you go** (see [Commit messages](#commit-messages)).
5. **Run the checks** from [the next section](#what-must-pass-before-you-open-a-pull-request) on your own machine.
6. **Open a pull request** against `main`. Fill in the pull request template. It has a checklist that matches this file.
7. **Wait for the automated checks.** GitHub Actions runs the Windows tests and builds the Windows installer, and builds the Android app. Please fix any red check. If one fails for a reason you think is not your change, say so in a comment.
8. **Respond to review.** Reviewers may ask for changes. That is normal and not a criticism. Push new commits to the same branch.
9. A maintainer merges the pull request when the checks are green and the review is done.

Pull requests that only change Markdown files or the `docs` folder do not trigger the build workflows.

## Commit messages

Look at `git log` and match it.

- Start with a **short summary in the imperative mood**: "Fix the Codex path lookup on Windows", not "Fixed" or "Fixes".
- Keep the first line under about 72 characters. No full stop at the end.
- Start with the area when it helps: `Desktop:`, `Android:`, `Codex:`, `Contract:`, `CI:`, `Docs:`.
- Use the body, after a blank line, to explain **why** the change is needed. The diff already shows what changed.
- Do not put secrets, real keys, real email addresses or real account IDs in a commit message or in the code. Fixtures use invented data.

## What must pass before you open a pull request

Run these from the repository root. CI runs the same things, on Windows.

| Check | Command |
|---|---|
| Install exact dependency versions | `npm ci` |
| Type check (every TypeScript package) | `npm run typecheck` |
| TypeScript tests | `npm test` |
| Build the statusline hook the desktop crate needs | `node apps/desktop/scripts/build-sidecar.mjs --debug` |
| Rust tests | `cargo test --workspace --no-fail-fast` |
| Rust lints, warnings are errors | `cargo clippy --workspace --all-targets -- -D warnings` |
| Rust formatting | `cargo fmt --all -- --check` |

Fix formatting with `cargo fmt --all`.

Extra rules for Rust code:

- The crates use `#![forbid(unsafe_code)]`. Do not add `unsafe`.
- Public items in `cooldown-core`, `cooldown-adapters` and `cooldown-sync` need doc comments (the crates use `#![warn(missing_docs)]`).
- CI runs on Windows. Code that only works on one operating system goes behind `cfg(...)`, and it must not break Clippy on the other systems.

For changes to the Android app, also build it: `npx tauri android build --debug --apk --target aarch64` in `apps/android` (see [docs/build-from-source.md](docs/build-from-source.md)).

## The data-contract rule

`packages/snapshot-schema` is the contract between every part of Cooldown: the Rust crates, the Windows app, the Android app and the shared screens. It defines `Snapshot`, `SyncPayload` and `AlertIntent`.

**If you change the shape of any of these, you must change all of the following in the same pull request:**

1. The TypeScript types: `packages/snapshot-schema/src/types.ts`.
2. The validators: `packages/snapshot-schema/src/schema.ts`.
3. The Rust model: `crates/cooldown-core/src/model.rs`.
4. The JSON Schema files in `packages/snapshot-schema/schema/`. Regenerate them with `npm run build:schema -w @cooldown/snapshot-schema` and commit the result. The tests fail if they are out of date.
5. The fixtures in `packages/snapshot-schema/fixtures/` (and the fixture README if the rules change). The TypeScript and Rust tests read the same fixture files, so they must agree.
6. Any screen or sync code that uses the changed field (`packages/ui`, `apps/desktop`, `apps/android`, `crates/cooldown-sync`).
7. [docs/pairing-protocol.md](docs/pairing-protocol.md) if the change affects what is sent between the PC and the phone.

A breaking change also means bumping `schemaVersion`. Additive, optional fields are preferred. When the Rust and TypeScript code disagree, the fixtures are the authority.

## Adding a provider adapter

An adapter reads one source (a file, a command, a web API) and turns it into `Snapshot`s. Existing adapters in `crates/cooldown-adapters/src` are good examples. `openrouter.rs` is the simplest API-key adapter.

The steps:

1. **Check it is allowed.** The source must be official or documented, or at least must not break the [project principles](#project-principles). If it needs a subscription login, a cookie or a private endpoint, the answer is no. If you are not sure, open an issue and ask first.
2. **Write the pure mapping function.** Every adapter has `map_input(&serde_json::Value, &MapContext) -> Vec<Snapshot>`. It takes the raw response and a context (the clock, the account label, the previous snapshots, the HTTP status) and returns snapshots. It does no network, no disk and no clock reads, so it is easy to test.
3. **Write the adapter.** Implement the `Adapter` trait from `cooldown-core` (`crates/cooldown-core/src/adapter.rs`): `id()`, `version()`, `poll_interval()` and `poll()`.
   - `poll` must **never return an error**. A failure becomes a `degraded` or `unavailable` snapshot that keeps the last good value (the helpers in `crates/cooldown-adapters/src/mapping.rs` do this).
   - Read API keys from the `SecretStore`, on every poll. Keep them in `SecretString`. Never log them or put them in a note.
   - Use the shared HTTP client in `crates/cooldown-adapters/src/http.rs`. It uses HTTPS only, caps the response size, refuses redirects to another origin and sets timeouts.
   - Notes must not contain text copied from the provider's response.
4. **Add fixtures.** In `packages/snapshot-schema/fixtures/adapters/<adapter-id>/`, add three files per case:
   - `<case>.input.json`: the raw provider response (invented data only),
   - `<case>.context.json`: the clock, account and previous snapshots,
   - `<case>.expected.json`: the exact snapshots the adapter must produce.

   Cover at least: a normal reading, an error response, a changed or unexpected response shape, and (if it has resets) a window that has just reset.
5. **Register it with the tests.**
   - Add the adapter to the table in `packages/snapshot-schema/src/adapters.ts`.
   - Add it to the `ADAPTERS` list in `crates/cooldown-adapters/tests/fixtures.rs`, and raise `MINIMUM_CASES` there.
   - Add a TypeScript reference mapping in `packages/snapshot-schema/test/reference-adapters.ts` if the existing tests need one.
6. **Wire it into the app.**
   - Add an entry to `CATALOG` and to `build()` in `apps/desktop/src-tauri/src/adapters.rs`.
   - Add a provider entry (name, "how this number is obtained", how often it updates) in `packages/ui/src/lib/providers.ts`.
7. **Credit anything you reused** in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
8. **Update the docs.** Add the provider to the table in [README.md](README.md) and to [docs/how-it-works.md](docs/how-it-works.md).
9. Run all the [checks](#what-must-pass-before-you-open-a-pull-request).

## Reusing other people's code

Cooldown is MIT licensed, and it reuses ideas and code from other open-source projects. That is welcome, with rules.

- Only reuse code under a permissive license that is compatible with MIT, such as MIT, Apache-2.0, BSD or the Unlicense. **Do not copy code under the GPL, AGPL or any license you are unsure about.** You may read such projects for ideas.
- Do not reuse anything that reads subscription logins, tokens or cookies, or calls private endpoints. See principle 3.
- **Add an entry to [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)** in the same pull request. Copy the layout of the existing entries: the project name and link, its license, where it is used in Cooldown, what you took or adapted, and the upstream copyright notice and license text. Apache-2.0 material keeps its license text and NOTICE file.
- Say whether the code is a copy, an adaptation or only an idea.

## Tests and test data

- A test must not use the real network, a real Claude or Codex installation, a real credential file, the real operating system keychain or the real `~/.claude` folder. Use temporary folders, a local mock HTTP server, the in-memory secret store and fake commands. The existing tests show how.
- All fixture data is **invented**. No real keys, tokens, email addresses, account IDs, team IDs or personal paths. Use things like `/home/example/...` and `00000000-...`.
- If a fixture has to contain something that looks like a secret to test redaction, it must be obviously fake. If a secret scanner complains, check with a maintainer before you add an ignore rule.
- Add or update tests with every bug fix and every new behavior.

## Changing the screens

The shared screens live in `packages/ui` and are used by both the Windows app and the Android app. Read [packages/ui/PRODUCT.md](packages/ui/PRODUCT.md) and [packages/ui/DESIGN.md](packages/ui/DESIGN.md) first.

- Before you say a screen is done, look at it at **390 px wide** (a phone) and at **1440 px wide** (a desktop), in light and dark themes.
- Respect `prefers-reduced-motion`.
- Screens must work with the keyboard and with a screen reader. Do not use colour as the only signal.
- Do not load fonts, scripts or images from the internet. The app works offline.
- Run the UI in a browser with sample data: `npm run dev -w @cooldown/ui`.

## Writing documentation

Most of our users are not programmers. Write for them.

- Plain English and short sentences. Explain any technical word the first time.
- Numbered steps for anything the reader must do.
- Use the **exact text of buttons and labels** from the app, in bold, so the reader can find them.
- Do not describe a feature that does not exist yet. If something is planned, say so.
- Never include personal data: your email address, your name in a file path, real keys.

## Security-sensitive changes

Extra care, and expect a closer review, if you touch any of these:

- pairing, encryption or syncing (`crates/cooldown-sync`, [docs/pairing-protocol.md](docs/pairing-protocol.md)),
- where or how keys are stored,
- the Claude Code hook install (it edits the user's `~/.claude/settings.json`),
- what is included in the payload sent to a phone,
- anything that opens a network port or starts another program.

Explain the security effect in your pull request. Never publish an exploitable bug in a public issue or pull request; use [SECURITY.md](SECURITY.md).

---

Questions? Open an issue and ask. A question is a perfectly good issue.
