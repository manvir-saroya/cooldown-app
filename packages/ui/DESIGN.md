---
name: Cooldown dashboard
description: A tide table for AI limits. Chart-water light theme and a night-chart dark theme, with certainty drawn as line form.
colors:
  chart-water: "oklch(0.962 0.014 225)"
  plate: "oklch(0.995 0.004 225)"
  chart-ink: "oklch(0.25 0.055 255)"
  ink-soft: "oklch(0.46 0.04 250)"
  hairline: "oklch(0.885 0.02 232)"
  tide-deep: "oklch(0.47 0.115 235)"
  tide: "oklch(0.6 0.11 220)"
  tide-soft: "oklch(0.93 0.035 215)"
  tide-ink: "oklch(0.42 0.09 235)"
  shoal: "oklch(0.55 0.2 350)"
  shoal-ink: "oklch(0.47 0.18 350)"
  sand: "oklch(0.72 0.1 85)"
  sand-soft: "oklch(0.95 0.045 90)"
  sand-ink: "oklch(0.45 0.085 70)"
  night-chart: "oklch(0.175 0.03 255)"
  night-plate: "oklch(0.215 0.032 255)"
  night-tide: "oklch(0.78 0.11 215)"
typography:
  page-title:
    fontFamily: "Schibsted Grotesk Variable, ui-sans-serif, system-ui, sans-serif"
    fontSize: "1.5rem"
    fontWeight: 600
    lineHeight: 1.25
  reading:
    fontFamily: "Schibsted Grotesk Variable, ui-sans-serif, system-ui, sans-serif"
    fontSize: "2.25rem"
    fontWeight: 600
    lineHeight: 1
  countdown:
    fontFamily: "Schibsted Grotesk Variable, ui-sans-serif, system-ui, sans-serif"
    fontSize: "1.5rem"
    fontWeight: 600
    lineHeight: 1.25
  body:
    fontFamily: "Schibsted Grotesk Variable, ui-sans-serif, system-ui, sans-serif"
    fontSize: "0.875rem"
    fontWeight: 400
    lineHeight: 1.5
  meta:
    fontFamily: "Schibsted Grotesk Variable, ui-sans-serif, system-ui, sans-serif"
    fontSize: "0.75rem"
    fontWeight: 500
    lineHeight: 1.33
rounded:
  sm: "4px"
  md: "8px"
  lg: "10px"
  xl: "14px"
spacing:
  sm: "8px"
  md: "16px"
  lg: "24px"
components:
  limit-plate:
    backgroundColor: "{colors.plate}"
    textColor: "{colors.chart-ink}"
    rounded: "{rounded.xl}"
    padding: "20px"
  bell-button-armed:
    backgroundColor: "{colors.tide-deep}"
    textColor: "{colors.plate}"
    rounded: "{rounded.lg}"
    height: "44px"
  source-badge-official:
    backgroundColor: "{colors.tide-soft}"
    textColor: "{colors.tide-ink}"
    rounded: "{rounded.md}"
    height: "24px"
  status-badge-stale:
    backgroundColor: "{colors.sand-soft}"
    textColor: "{colors.sand-ink}"
    rounded: "{rounded.md}"
    height: "24px"
---

# Design System: Cooldown dashboard

## Overview

**Creative North Star: "The Tide Table"**

A usage limit behaves like a tide: it drains, it has a predicted turn, and it comes back at a known time. The interface reads like a tide table and a nautical chart. Predicted and observed figures are drawn differently, every number carries its source and age, and the reset time is the loudest thing on a plate.

It is an Operate surface: calm, dense enough to scan nine limits at once, and honest. Colour is restrained to ink on pale chart water with one live accent (tide) and two warning inks. Certainty is carried by line form (solid, hatched, dashed, struck) so it survives without colour.

**Key Characteristics:**
- One grotesque, tabular numerals everywhere a number can change.
- Plates (bordered white panels), not shadows, for elevation; a plate lifts slightly under the pointer.
- A graduated "tide staff" meter with ticks every 10 percent.
- Dark theme is a night chart, not an inverted light theme.

## Colors

Cool, slightly blue neutrals with a teal-blue live accent. Magenta and ochre are reserved for meaning.

### Primary
- **Tide Deep** (oklch(0.47 0.115 235)): primary actions and the armed bell. In dark theme it becomes bright **Night Tide** (oklch(0.78 0.11 215)).
- **Tide** (oklch(0.6 0.11 220)): the meter fill and chart line for a healthy, measured reading.

### Secondary
- **Shoal** (oklch(0.55 0.2 350), ink oklch(0.47 0.18 350)): only for a window at or above 85 percent and the "Limit reached" state. Never decoration.
- **Sand** (oklch(0.72 0.1 85), ink oklch(0.45 0.085 70)): uncertainty. Stale, degraded and estimated readings, and the hatch pattern.

### Neutral
- **Chart Water** (oklch(0.962 0.014 225)): page ground. **Plate** (oklch(0.995 0.004 225)): cards and panels. **Chart Ink** (oklch(0.25 0.055 255)): text. **Ink Soft** (oklch(0.46 0.04 250)): secondary text. **Hairline** (oklch(0.885 0.02 232)): borders.
- Dark: **Night Chart** (oklch(0.175 0.03 255)) ground, **Night Plate** (oklch(0.215 0.032 255)) panels.

### Named Rules
**The Not-By-Colour-Alone Rule.** Every status and source has an icon and a word, and certainty also changes line form. Magenta and ochre never carry meaning alone.
**The Reserved Magenta Rule.** Shoal is spent only on "nearly out" and "limit reached". If it appears anywhere else, it has stopped meaning anything.

## Typography

**Font:** Schibsted Grotesk (variable, self-hosted through Fontsource so the app works offline), falling back to the system sans.

**Character:** a news grotesque with plain, unslashed numerals and distinct 1/I/l. One family carries headings, labels, data and prose.

### Hierarchy
- **Page title** (600, 1.5rem on desktop, 1.125rem on mobile): the route name in the top bar.
- **Reading** (600, 2.25rem, line-height 1): the percent used or the balance on a plate.
- **Countdown** (600, 1.5rem): the live time to reset, tabular numerals, never wraps.
- **Body** (400, 0.875rem to 1rem, 1.5): descriptions and notes, capped near 65ch with `max-w-prose`.
- **Meta** (500, 0.75rem): badges, ages, scale labels. Nothing is set below 12px.

### Named Rules
**The Tabular Rule.** Anything that changes while you watch uses `.num` (tabular, lining numerals) so it never jitters.

## Layout

Mobile first at 390px: one column of plates, a thumb-reach bottom navigation, 16px gutters. From 1024px a 16rem sidebar frames an inset content panel with its own scroll area. The overview grid is 1 column on phones, 2 from 640px and 3 from 1280px. Settings splits into two columns from 1280px. Content is capped at 72rem. Spacing follows a 4px base with 16px between plates and 24 to 32px between sections.

## Elevation & Depth

Flat at rest. Depth comes from 1px hairline borders on plates and a tonal step between ground, sidebar and plate. A plate under the pointer lifts 2px with a soft shadow (`.plate-lift`); nothing has a shadow at rest. Overlays (dialogs, menus) use a scrim and the popover surface.

### Named Rules
**The One Edge Rule.** At rest a surface has either a border or a shadow, never both. The hover lift is the one exception.

## Shapes

Plates use a 14px radius, controls 8 to 10px, and the meter and badges 4 to 8px, so the staff reads as a ruler rather than a pill. Provider marks are neutral rounded-square monograms, not brand logos.

## Components

### Tide hero (overview, added 2026-10-25)
The soonest reset leads the overview as a large rolling countdown over a band of water. The water's height is the share of the window that has passed, so it is full when the limit comes back. Two wave layers drift sideways and a live dot pulses; both stand still under reduced motion. Digits that change slide in, and each one is plain text, so the timer reads, copies and searches normally.

### Pace marker
A row meter can carry a thin marker at the share of the window that has passed: the point an even spend would have reached by now. Under the meter one line says "N% in reserve", "on an even pace" or "N% ahead of an even pace" (tide ink, quiet, ochre). Idea from CodexBar's even-use guide (https://github.com/steipete/CodexBar). Hidden when the limit is reached or the reading is unavailable.

### Provider hue
A provider's hue (`--mark-h`) tints its monogram as a gradient tile and lays a faint wash in the top-left corner of its panel and balance tile. It tells providers apart; it never carries status.

### Limit plate
Provider mark, name and window, a source badge, one line of provenance or a tinted problem notice, the reading, the reset countdown, the tide staff, a status badge with the observed age, and the bell button. The title is a stretched link to the provider detail; the bell sits above it.

### Tide staff (meter)
A 14px bar with ticks every 10 percent and a 0 / 50 / 100% scale. Solid fill is a measured reading. Hatched fill with a dashed outline is approximate (stale, degraded, estimated). Hatch plus a diagonal strike is unavailable, with the last good value kept beside it. The fill is revealed by a spring-driven clip so patterns do not stretch.

### Source badge
Icon plus word. Official is a solid outline, estimated dashed, scraped dotted, manual a plain neutral outline.

### Bell button
Outline until armed, then a filled tide-deep button with a ringing bell icon. The label never changes ("Notify me when it resets"); `aria-pressed` and the line beneath ("Bell rings at 8:06 PM, in 4m 40s") carry the state.

### Navigation
Sidebar from 1024px (nav, providers with a status word); below that a bottom bar of four items with a sliding active pill and a bell-count badge.

## Do's and Don'ts

### Do:
- **Do** keep the last good value visible on every non-ok state and say why in a tinted notice.
- **Do** show source and age next to every figure.
- **Do** keep touch targets at 44px or more below 1024px.
- **Do** use Motion for movement and respect `prefers-reduced-motion`: scale and slide entrances become fades.

### Don't:
- **Don't** use magenta or ochre for anything but the states above.
- **Don't** add shadows to plates; use the hairline.
- **Don't** hand-write easing curves; use Motion springs or the shadcn defaults.
- **Don't** use a kicker or eyebrow label above a heading.
