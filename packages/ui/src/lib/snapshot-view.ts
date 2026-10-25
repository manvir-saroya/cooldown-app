/**
 * View helpers for Snapshot data. Pure functions, no React, no clock access:
 * every function that needs "now" takes it as an argument so it is testable
 * and so one shared ticker can drive every countdown on screen.
 *
 * These deliberately do not depend on @cooldown/snapshot-schema runtime code
 * (only its types), so the UI builds even while that package is in flux.
 */
import type { AlertIntent, Snapshot, Status, Unit, WindowKind } from '@cooldown/snapshot-schema'

const SECOND = 1000
const MINUTE = 60 * SECOND
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

/** Higher is worse. The effective status never gets better than the reported one. */
export const SEVERITY: Record<Status, number> = { ok: 0, stale: 1, degraded: 2, unavailable: 3 }

export type EffectiveStatus = {
  status: Status
  /** True when resetsAt is in the past, so the value describes a window that has already ended. */
  resetPassed: boolean
}

/**
 * Spec section 4: a snapshot whose resetsAt is in the past is stale for every
 * consumer. A snapshot that is already degraded or unavailable keeps that
 * (more informative) status.
 */
export function effectiveStatus(snapshot: Snapshot, now: number): EffectiveStatus {
  const resetsAt = parseTime(snapshot.resetsAt)
  const resetPassed = resetsAt !== undefined && resetsAt <= now
  const reported = snapshot.status
  const status: Status = resetPassed && SEVERITY[reported] < SEVERITY.stale ? 'stale' : reported
  return { status, resetPassed }
}

export function parseTime(iso: string | undefined): number | undefined {
  if (!iso) return undefined
  const t = Date.parse(iso)
  return Number.isNaN(t) ? undefined : t
}

/** "just now", "2 min ago", "3 h ago", "2 d ago". Used for "Last observed ...". */
export function formatAge(ms: number): string {
  if (!Number.isFinite(ms)) return 'never'
  if (ms < 45 * SECOND) return 'just now'
  if (ms < 90 * SECOND) return '1 min ago'
  if (ms < 45 * MINUTE) return `${Math.round(ms / MINUTE)} min ago`
  if (ms < 90 * MINUTE) return '1 h ago'
  if (ms < 22 * HOUR) return `${Math.round(ms / HOUR)} h ago`
  if (ms < 36 * HOUR) return '1 d ago'
  return `${Math.round(ms / DAY)} d ago`
}

/** An observation older than this is flagged as old even if the adapter said "ok". */
export const OLD_OBSERVATION_MS = 30 * MINUTE

export type CountdownParts = {
  /** Milliseconds remaining, clamped at 0. */
  remainingMs: number
  days: number
  hours: number
  minutes: number
  seconds: number
  /** True once the target time has been reached. */
  done: boolean
}

export function countdownParts(targetMs: number, now: number): CountdownParts {
  const remainingMs = Math.max(0, targetMs - now)
  // Round up to the next whole second so "0s" is only shown once the target is reached.
  const totalSeconds = Math.ceil(remainingMs / SECOND)
  return {
    remainingMs,
    days: Math.floor(totalSeconds / 86400),
    hours: Math.floor((totalSeconds % 86400) / 3600),
    minutes: Math.floor((totalSeconds % 3600) / 60),
    seconds: totalSeconds % 60,
    done: remainingMs === 0,
  }
}

/**
 * Compact countdown. Two units at most so it never reflows while ticking:
 * "3d 5h", "2h 17m", "4m 32s", "9s". Seconds only appear below one hour.
 */
export function formatCountdown(targetMs: number, now: number): string {
  const p = countdownParts(targetMs, now)
  if (p.done) return 'now'
  if (p.days > 0) return `${p.days}d ${p.hours}h`
  if (p.hours > 0) return `${p.hours}h ${pad2(p.minutes)}m`
  if (p.minutes > 0) return `${p.minutes}m ${pad2(p.seconds)}s`
  return `${p.seconds}s`
}

/** Spoken form for assistive tech: "4 minutes 32 seconds". Rounded to the minute above one hour. */
export function countdownSpoken(targetMs: number, now: number): string {
  const p = countdownParts(targetMs, now)
  if (p.done) return 'now'
  const unit = (n: number, word: string) => `${n} ${word}${n === 1 ? '' : 's'}`
  if (p.days > 0) return `${unit(p.days, 'day')} ${unit(p.hours, 'hour')}`
  if (p.hours > 0) return `${unit(p.hours, 'hour')} ${unit(p.minutes, 'minute')}`
  if (p.minutes > 0) return `${unit(p.minutes, 'minute')}`
  return unit(p.seconds, 'second')
}

function pad2(n: number): string {
  return n < 10 ? `0${n}` : String(n)
}

/** Percent used 0..100, from percentUsed or used/limit. Undefined when there is nothing to draw. */
export function percentUsed(snapshot: Snapshot): number | undefined {
  if (typeof snapshot.percentUsed === 'number') return clampPercent(snapshot.percentUsed)
  if (typeof snapshot.used === 'number' && typeof snapshot.limit === 'number' && snapshot.limit > 0) {
    return clampPercent((snapshot.used / snapshot.limit) * 100)
  }
  if (typeof snapshot.remaining === 'number' && typeof snapshot.limit === 'number' && snapshot.limit > 0) {
    return clampPercent(((snapshot.limit - snapshot.remaining) / snapshot.limit) * 100)
  }
  return undefined
}

export function clampPercent(n: number): number {
  return Math.min(100, Math.max(0, n))
}

export type MeterTone = 'calm' | 'high' | 'full'

/** How close to the limit a window is. Drives colour and wording together. */
export function meterTone(percent: number | undefined): MeterTone {
  if (percent === undefined) return 'calm'
  if (percent >= 100) return 'full'
  if (percent >= 85) return 'high'
  return 'calm'
}

export function alertIdFor(snapshot: Pick<Snapshot, 'provider' | 'account' | 'window'>): string {
  return `reset:${snapshot.provider}:${snapshot.account}:${snapshot.window}`
}

export function snapshotIdFromAlertId(alertId: string): string | undefined {
  return alertId.startsWith('reset:') ? alertId.slice('reset:'.length) : undefined
}

/** True when the snapshot has a future reset an alert could ring at. */
export function canAlert(snapshot: Snapshot, now: number): boolean {
  const resetsAt = parseTime(snapshot.resetsAt)
  return resetsAt !== undefined && resetsAt > now
}

export function makeAlertIntent(snapshot: Snapshot): AlertIntent | undefined {
  if (!snapshot.resetsAt) return undefined
  return { id: alertIdFor(snapshot), fireAt: snapshot.resetsAt, enabled: true }
}

const WINDOW_LABELS: Record<WindowKind, string> = {
  session_5h: '5-hour session',
  weekly: 'Weekly',
  monthly: 'Monthly',
  daily: 'Daily',
  balance: 'Balance',
}

export function windowLabel(window: WindowKind): string {
  return WINDOW_LABELS[window]
}

/** Length of a rolling window in ms, when it has a fixed one. */
export function windowLengthMs(window: WindowKind): number | undefined {
  switch (window) {
    case 'session_5h':
      return 5 * HOUR
    case 'weekly':
      return 7 * DAY
    case 'daily':
      return DAY
    case 'monthly':
      return 30 * DAY
    default:
      return undefined
  }
}

/**
 * How a window's use compares with spending it evenly: `elapsed` is the percent of the window that has
 * passed, `delta` is percent used minus that (positive means faster than even). Whole percents, so a
 * selector built on it changes about once a minute. Undefined without a fixed window or a reset time.
 */
export function windowPace(snapshot: Snapshot, now: number): { elapsed: number; delta: number } | undefined {
  const length = windowLengthMs(snapshot.window)
  const resetsAt = parseTime(snapshot.resetsAt)
  const percent = percentUsed(snapshot)
  if (length === undefined || resetsAt === undefined || percent === undefined || resetsAt <= now) return undefined
  const elapsed = Math.round(clampPercent(100 - ((resetsAt - now) / length) * 100))
  return { elapsed, delta: Math.round(percent) - elapsed }
}

/** The history section's heading. Known from the snapshot, so it stays the same while the chart loads. */
export function historyTitle(snapshot: Snapshot): string {
  return snapshot.window === 'balance' || isMoneyUnit(snapshot.unit) ? 'Balance history' : 'Usage history'
}

/** Whether a unit is an amount of money (a balance). */
export function isMoneyUnit(unit: Unit): boolean {
  return unit === 'usd' || unit === 'cny'
}

/** An amount in its currency: `$12.34`, or `CN¥110.00` for yuan. Units that are not money are shown as dollars. */
export function formatMoney(value: number, unit: Unit, fractionDigits = 2): string {
  return new Intl.NumberFormat('en-US', {
    style: 'currency',
    currency: unit === 'cny' ? 'CNY' : 'USD',
    minimumFractionDigits: fractionDigits,
    maximumFractionDigits: fractionDigits,
  }).format(value)
}

const timeFormat = new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit' })
const weekdayTimeFormat = new Intl.DateTimeFormat(undefined, {
  weekday: 'short',
  hour: '2-digit',
  minute: '2-digit',
})
const dateTimeFormat = new Intl.DateTimeFormat(undefined, {
  month: 'short',
  day: 'numeric',
  hour: '2-digit',
  minute: '2-digit',
})

/** Midnight, local time, of the day that contains `ms`. */
export function startOfDay(ms: number): number {
  const d = new Date(ms)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

/**
 * Wall-clock label for a moment, relative to today:
 * "14:32", "Tomorrow 09:10", "Sat 18:40", "Nov 3, 07:00".
 */
export function formatClock(targetMs: number, now: number): string {
  const dayDiff = Math.round((startOfDay(targetMs) - startOfDay(now)) / DAY)
  if (dayDiff === 0) return timeFormat.format(targetMs)
  if (dayDiff === 1) return `Tomorrow ${timeFormat.format(targetMs)}`
  if (dayDiff > 1 && dayDiff < 7) return weekdayTimeFormat.format(targetMs)
  return dateTimeFormat.format(targetMs)
}

/** Full local timestamp for tooltips and detail screens. */
export function formatFull(targetMs: number): string {
  return new Intl.DateTimeFormat(undefined, {
    weekday: 'short',
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  }).format(targetMs)
}

/** The snapshot whose reset comes soonest in the future, if any. */
export function nextReset(snapshots: Snapshot[], now: number): Snapshot | undefined {
  let best: Snapshot | undefined
  let bestAt = Number.POSITIVE_INFINITY
  for (const s of snapshots) {
    const at = parseTime(s.resetsAt)
    if (at !== undefined && at > now && at < bestAt) {
      best = s
      bestAt = at
    }
  }
  return best
}

/** The one-line answer to "can I use this provider now?", from all of its limit windows. */
export type Verdict =
  /** A window is used up. `until` is when the last used-up window resets (unknown when none says). */
  | { kind: 'blocked'; until: number | undefined }
  /** Usable, but a window is past the warning line. */
  | { kind: 'low'; window: WindowKind; percent: number }
  | { kind: 'ready' }
  /** No window has a reading yet. */
  | { kind: 'unknown' }

/** At or above this a window counts as running low (the same line the meter turns at). */
export const LOW_AT_PERCENT = 85

export function providerVerdict(windows: Snapshot[], now: number): Verdict {
  const read = windows
    .filter((s) => s.window !== 'balance')
    .map((s) => ({ s, percent: percentUsed(s), resetsAt: parseTime(s.resetsAt) }))
    .filter((w) => w.percent !== undefined)
  if (read.length === 0) return { kind: 'unknown' }
  // A window whose reset time has passed has come back, whatever its last figure said.
  const live = read.filter((w) => w.resetsAt === undefined || w.resetsAt > now)
  const full = live.filter((w) => w.percent! >= 100)
  if (full.length > 0) {
    const times = full.map((w) => w.resetsAt).filter((t): t is number => t !== undefined)
    return { kind: 'blocked', until: times.length === full.length ? Math.max(...times) : undefined }
  }
  const worst = live.reduce<(typeof live)[number] | undefined>((a, w) => (a && a.percent! >= w.percent! ? a : w), undefined)
  if (worst && worst.percent! >= LOW_AT_PERCENT) return { kind: 'low', window: worst.s.window, percent: worst.percent! }
  return { kind: 'ready' }
}

/** Snapshots grouped per provider and account, in overview order. Balances are kept apart. */
export function groupForOverview(snapshots: Snapshot[]): {
  limits: { key: string; provider: string; account: string; windows: Snapshot[] }[]
  balances: Snapshot[]
} {
  const limits: { key: string; provider: string; account: string; windows: Snapshot[] }[] = []
  const balances: Snapshot[] = []
  for (const s of sortSnapshots(snapshots)) {
    if (s.window === 'balance' || isMoneyUnit(s.unit)) {
      balances.push(s)
      continue
    }
    const key = `${s.provider}:${s.account}`
    const group = limits.find((g) => g.key === key)
    if (group) group.windows.push(s)
    else limits.push({ key, provider: s.provider, account: s.account, windows: [s] })
  }
  return { limits, balances }
}

/** Stable provider ordering for the overview grid. */
const PROVIDER_ORDER = ['claude', 'codex', 'openrouter', 'deepseek', 'xai']
const WINDOW_ORDER: WindowKind[] = ['session_5h', 'daily', 'weekly', 'monthly', 'balance']

export function sortSnapshots(snapshots: Snapshot[]): Snapshot[] {
  const pIdx = (p: string) => {
    const i = PROVIDER_ORDER.indexOf(p)
    return i === -1 ? PROVIDER_ORDER.length : i
  }
  return [...snapshots].sort(
    (a, b) =>
      pIdx(a.provider) - pIdx(b.provider) ||
      a.provider.localeCompare(b.provider) ||
      WINDOW_ORDER.indexOf(a.window) - WINDOW_ORDER.indexOf(b.window),
  )
}

export function snapshotsForProvider(snapshots: Snapshot[], provider: string): Snapshot[] {
  return sortSnapshots(snapshots.filter((s) => s.provider === provider))
}
