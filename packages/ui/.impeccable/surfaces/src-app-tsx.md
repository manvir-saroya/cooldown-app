---
version: 1
slug: "src-app-tsx"
primary_target: "src/App.tsx"
related_targets: []
---

# Surface brief: Cooldown dashboard (all screens)

Mode: Operate. Scope: Overview, Provider detail, Alerts, Settings, Pairing in one app shell (Capitalio shell, Watermelon limit cards).
Audience/job: developers checking "how much is left and when does it come back" on a 390px phone or a desktop sidebar layout.
Constraints: registry-first components, offline fonts, light and dark, reduced motion, 44px touch targets. Sample data only, labelled synthetic.
Unattended run: no user decision round was possible; the roll's assigned direction was built and this is disclosed in the report.

## Direction contract

THESIS: A limit is a tide, not a gauge: it drains, has a predicted turn, and comes back at a known time. The surface refuses the generic usage-meter dashboard (one blue accent, rounded identical stat cards) and reads like a tide table and nautical chart where predicted and observed figures are drawn differently.
OWN-WORLD: Pale chart-water ground and white card plates in light, an ECDIS-style night chart in dark. Deep chart-navy ink, tide cyan-teal for the live/ok line, chart magenta only for a near-exhausted window, sand ochre for stale. Meters are graduated tide staffs (ticks every 10 percent, a high-water notch). Certainty is line form: solid = surveyed (official), dashed outline plus hatch fill = approximate (estimated, stale, degraded), struck line = unavailable. One grotesque (Schibsted Grotesk, self-hosted variable font) with tabular numerals everywhere; chosen over Atkinson Hyperlegible Next because its slashed zero made times and percentages noisy.
STORY: The visitor sees at once which windows are open, how full each is, and in what time each turns; trusts each number because its source and age sit beside it; and sets a reset bell with one tap.
FIRST VIEWPORT: Desktop: sidebar rail left, Overview header with a "next reset" line (soonest window, live countdown), then a 2-3 column grid of limit plates, each with provider, window, staff meter, large percent, countdown, source mark, age, bell toggle. Phone: header, next-reset strip, single column of plates, fixed bottom navigation.
FORM: Seed 67c86e6c, assigned index 4 of the grounded list: (1) departures board, (2) terminal TUI, (3) instrument cluster, (4) tide table and nautical chart, (5) dose schedule, (6) phone OS widgets, (7) rail signal diagram. Built: 4.
FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance

## Challenger verdicts
- emission-line rail: competitive on one axis (state as line form); kept as a raise: certainty encoded in dash, hatch and strike, never hue alone.
- jet-age ticket wallet: declined; kept: a cancelled value stays visible but struck (unavailable keeps the last good number).
- Saville catalog sleeve: declined (coded strips fail product clarity).
- midnight transit diagram: declined on clarity for five limits; kept: arriving/delayed vocabulary for countdown and stale copy.
- labanotation: declined.
- Japanese high-density: declined; kept: provenance metadata is small but always present.
