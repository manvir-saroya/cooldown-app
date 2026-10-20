<!--
Thank you for contributing! Please read CONTRIBUTING.md first.
Docs-only changes do not run the build checks; everything else does.
-->

## What does this change, and why?

<!-- A few plain sentences. Link the issue it fixes: "Fixes #123". For a big change, open an issue first. -->

## How did you test it?

<!-- Commands you ran, devices you tried it on (Windows version, Android version), screenshots for screens (390 px and 1440 px, light and dark). -->

## Checklist

**Rules** (see CONTRIBUTING.md (project principles))

- [ ] This change keeps Cooldown local-first: no hosted service, no account, no relay.
- [ ] It never reads subscription OAuth tokens, credential files or cookies, and calls no private usage endpoint.
- [ ] API keys stay on the device: not logged, not in a snapshot, not synced, not in an error message.
- [ ] Every number it shows still has its source and its age.
- [ ] No telemetry or tracking was added.
- [ ] Code I reused from another project is credited in `THIRD_PARTY_NOTICES.md` (or I reused none).

**Checks** (run from the repository root; CI runs the same)

- [ ] `npm run typecheck`
- [ ] `npm test`
- [ ] `node apps/desktop/scripts/build-sidecar.mjs --debug`, then `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo fmt --all -- --check`

**If this applies**

- [ ] **Data contract changed** (`packages/snapshot-schema`): I updated the TypeScript types, validators, the Rust model in `cooldown-core`, the generated JSON Schema (`npm run build:schema -w @cooldown/snapshot-schema`) and the fixtures together, and bumped `schemaVersion` if the change is breaking.
- [ ] **New or changed adapter**: it has fixtures (`input`, `context`, `expected`), it is registered in the fixture tests, and `poll` never returns an error.
- [ ] **Screens changed**: I looked at them at 390 px and 1440 px, in light and dark, and they respect reduced motion.
- [ ] **Pairing, sync, key storage or the hook install changed**: I explained the security effect above, and updated `docs/pairing-protocol.md` if needed.
- [ ] **Behavior or buttons changed**: I updated the docs (`README.md`, `docs/`).
- [ ] This is not a security vulnerability report. (Those go through `SECURITY.md`, privately.)

I agree that my contribution is released under the project's MIT license and that I will follow the Code of Conduct (CODE_OF_CONDUCT.md).
