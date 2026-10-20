/**
 * Deterministic synthetic history for the charts. The same inputs always give
 * the same curve so screenshots and tests are stable.
 */
import type { Snapshot } from '@cooldown/snapshot-schema'
import { HISTORY_RANGES, type HistoryPoint, type HistoryRange, type HistorySeries } from './types'

const HOUR = 3_600_000
const DAY = 24 * HOUR

function mulberry32(seed: number) {
  let a = seed >>> 0
  return () => {
    a |= 0
    a = (a + 0x6d2b79f5) | 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

function hashString(s: string): number {
  let h = 2166136261
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i)
    h = Math.imul(h, 16777619)
  }
  return h >>> 0
}

/** Monotone 0..1 curve for one window, built from random bursts so it looks like real work. */
function windowCurve(seed: number, bins = 24): number[] {
  const rnd = mulberry32(seed)
  const steps = Array.from({ length: bins }, () => (rnd() < 0.35 ? 0.05 : 0.3 + rnd() * 1.2))
  const total = steps.reduce((a, b) => a + b, 0)
  let acc = 0
  return [0, ...steps.map((s) => (acc += s / total))]
}

function sample(curve: number[], f: number): number {
  const x = Math.min(1, Math.max(0, f)) * (curve.length - 1)
  const i = Math.floor(x)
  const next = curve[Math.min(curve.length - 1, i + 1)] ?? 1
  const base = curve[i] ?? 0
  return base + (next - base) * (x - i)
}

export function buildSampleHistory(
  snapshot: Snapshot,
  range: HistoryRange,
  now: number,
): HistorySeries {
  const spanMs = HISTORY_RANGES.find((r) => r.value === range)?.ms ?? DAY
  const seed = hashString(snapshot.id)
  const start = now - spanMs
  const stepMs = range === '24h' ? 10 * 60_000 : range === '7d' ? HOUR : 4 * HOUR

  if (snapshot.window === 'balance') return buildBalanceHistory(snapshot, range, now, start, stepMs, seed)

  const length = snapshot.window === 'session_5h' ? 5 * HOUR : snapshot.window === 'weekly' ? 7 * DAY : DAY
  const anchor = Date.parse(snapshot.resetsAt ?? '') || now + length
  const current = snapshot.percentUsed ?? 0
  const points: HistoryPoint[] = []
  const resets = new Set<number>()

  // Window k covers (anchor - (k+1)*length, anchor - k*length]. k = 0 is the current window.
  const currentFraction = Math.min(0.999, Math.max(0.02, 1 - (anchor - now) / length))
  const currentCurve = windowCurve(seed)
  const currentScale = current / Math.max(0.05, sample(currentCurve, currentFraction))

  for (let t = start; t < now; t += stepMs) {
    const k = Math.max(0, Math.floor((anchor - t) / length))
    const windowStart = anchor - (k + 1) * length
    const f = (t - windowStart) / length
    let value: number
    if (k === 0) {
      value = sample(currentCurve, f) * currentScale
    } else {
      const rnd = mulberry32(seed + k * 7919)
      const peak = 45 + rnd() * 55
      value = sample(windowCurve(seed + k * 104729), f) * peak
    }
    points.push({ t, value: Math.min(100, Math.max(0, Math.round(value * 10) / 10)) })
  }
  // Mark every boundary that falls inside the range.
  for (let k = 0; ; k++) {
    const boundary = anchor - (k + 1) * length
    if (boundary < start) break
    if (boundary <= now) resets.add(boundary)
  }
  // Pin the final point to the snapshot's own number so the chart never disagrees with the card.
  points.push({ t: now, value: current })
  return {
    snapshotId: snapshot.id,
    unit: snapshot.unit,
    kind: 'usage',
    range,
    points,
    resets: [...resets].sort((a, b) => a - b),
  }
}

function buildBalanceHistory(
  snapshot: Snapshot,
  range: HistoryRange,
  now: number,
  start: number,
  stepMs: number,
  seed: number,
): HistorySeries {
  const rnd = mulberry32(seed)
  const end = snapshot.remaining ?? 0
  const observed = Date.parse(snapshot.observedAt) || now
  const spend: number[] = []
  const times: number[] = []
  for (let t = start; t < observed; t += stepMs) {
    times.push(t)
    spend.push((rnd() < 0.3 ? 0.1 : 0.4 + rnd() * 1.6) * (stepMs / (4 * HOUR)))
  }
  // Walk backwards from the current balance so the series ends exactly on it.
  const values = new Array<number>(times.length)
  let running = end
  for (let i = times.length - 1; i >= 0; i--) {
    values[i] = Math.round(running * 100) / 100
    running += spend[i] ?? 0
  }
  const points = times.map((t, i) => ({ t, value: values[i] ?? end }))
  points.push({ t: observed, value: end })
  return {
    snapshotId: snapshot.id,
    unit: snapshot.unit,
    kind: 'balance',
    range,
    points,
    resets: [],
  }
}
