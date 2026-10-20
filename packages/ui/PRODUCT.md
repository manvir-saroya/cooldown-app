# Product

<!-- impeccable:product-schema 1 -->

> Written unattended from the design spec (`docs/superpowers/specs/2026-10-10-cooldown-design.md`) and the build brief. No user interview was possible in this run; every fact below is taken from those two documents, and items marked (assumed) are inferences to confirm.

## Platform

web

## Stack

Fixed by the spec and the user's standing rules: React 19, TypeScript, Vite, Tailwind v4, shadcn/ui (radix), Motion, Recharts, qrcode.react. Components come from the Watermelon registry (Capitalio shell, Credit Usage Card, Budget Card) and shadcn before anything is hand-built. The same UI ships in a Tauri 2 window, a 127.0.0.1 browser tab and a Tauri Android app, so it must work offline with no CDN assets.

## Users

Developers who pay for Claude Code and Codex plans and use API-key providers (OpenRouter, DeepSeek, xAI). They hit 5-hour and weekly limits mid-task and want to know how much is left, when it comes back, and to be told the moment they can work again. They look at it on a Windows desktop beside their editor and on a phone (390px) while away from the desk. (assumed: they are comfortable with local-first, no-account tools.)

## Product Purpose

Show how much of each AI limit is used and exactly when it resets, label every number with where it came from and how old it is, and let the user ask for a "your limit is back" alert. Success: a glance answers "can I work now, and if not, in how long".

## Positioning

Local-first and honest. No hosted service, no account, keys never leave the device they were typed on, only sanitized snapshots sync between paired devices. Every figure carries a source (official, estimated, scraped, manual) and an age; failure never blanks a value, it marks the last good one stale or degraded.

## Operating Context

Data arrives as versioned Snapshot objects (spec section 4). A snapshot whose resetsAt has passed is stale for every consumer. Claude Code figures are only fresh while Claude Code runs. Codex figures come from an undocumented source and may degrade. Per-model Claude windows are never guessed; they read "not available".

## Capabilities and Constraints

v1 screens: Overview, Provider detail (history chart, reset schedule, source details), Alerts, Settings (adapters, API keys, Claude Code statusline hook, paired devices, theme), Pairing (QR plus manual code, local network only). Light and dark themes, user-toggleable and following system. Mobile-first at 390px with a sidebar layout from 1024px. prefers-reduced-motion respected. This package renders sample data only; the data seam (`SnapshotSource`) is later backed by Tauri invoke or local HTTP.

## Brand Commitments

Working name "Cooldown". Reuse plan (spec section 13): Watermelon Capitalio shell, Credit Usage Card and Budget Card, shadcn Progress, Sonner, Recharts, qrcode.react; only the mobile bottom navigation is hand-built.

## Evidence on Hand

No real usage data. All snapshots in this package are synthetic sample data and must stay labelled as such in the UI. Nothing here may claim a provider capability the spec does not list.

## Product Principles

1. Never blank a value: keep the last good number visible and mark it.
2. Provenance on every figure: source kind and age travel with the number.
3. The reset moment is the headline: countdowns are live and exact.
4. Secrets are local: say so wherever a key is typed.
5. Say "not available" rather than guess.

## Accessibility & Inclusion

Keyboard operable throughout, visible focus, 44px touch targets on mobile, status never conveyed by color alone, reduced motion respected.
