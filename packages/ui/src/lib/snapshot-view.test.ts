import type { Snapshot } from '@cooldown/snapshot-schema'
import { describe, expect, it } from 'vitest'
import {
  alertIdFor,
  canAlert,
  countdownSpoken,
  effectiveStatus,
  formatAge,
  formatCountdown,
  formatMoney,
  isMoneyUnit,
  makeAlertIntent,
  meterTone,
  nextReset,
  groupForOverview,
  percentUsed,
  providerVerdict,
  sortSnapshots,
  windowPace,
} from './snapshot-view'

const NOW = Date.parse('2026-10-10T12:00:00Z')
const MIN = 60_000
const HOUR = 60 * MIN
const DAY = 24 * HOUR

function snap(over: Partial<Snapshot> = {}): Snapshot {
  return {
    schemaVersion: 1,
    id: 'claude:personal:session_5h',
    provider: 'claude',
    account: 'personal',
    window: 'session_5h',
    unit: 'percent',
    percentUsed: 50,
    resetsAt: new Date(NOW + HOUR).toISOString(),
    observedAt: new Date(NOW - MIN).toISOString(),
    source: { kind: 'official', adapter: 'a', adapterVersion: '1' },
    status: 'ok',
    ...over,
  }
}

describe('effectiveStatus', () => {
  it('keeps ok while the reset is in the future', () => {
    expect(effectiveStatus(snap(), NOW)).toEqual({ status: 'ok', resetPassed: false })
  })
  it('treats an ok snapshot whose resetsAt has passed as stale (spec section 4)', () => {
    const s = snap({ resetsAt: new Date(NOW - 1000).toISOString() })
    expect(effectiveStatus(s, NOW)).toEqual({ status: 'stale', resetPassed: true })
  })
  it('never improves a worse reported status', () => {
    const past = new Date(NOW - 1000).toISOString()
    expect(effectiveStatus(snap({ status: 'degraded', resetsAt: past }), NOW).status).toBe('degraded')
    expect(effectiveStatus(snap({ status: 'unavailable', resetsAt: past }), NOW).status).toBe('unavailable')
  })
  it('is not affected by snapshots without a reset time', () => {
    expect(effectiveStatus(snap({ resetsAt: undefined }), NOW).status).toBe('ok')
  })
})

describe('formatAge', () => {
  it.each([
    [10_000, 'just now'],
    [60_000, '1 min ago'],
    [2 * MIN, '2 min ago'],
    [27 * MIN, '27 min ago'],
    [60 * MIN, '1 h ago'],
    [3 * HOUR, '3 h ago'],
    [30 * HOUR, '1 d ago'],
    [5 * DAY, '5 d ago'],
    [Number.POSITIVE_INFINITY, 'never'],
  ])('%d ms -> %s', (ms, label) => {
    expect(formatAge(ms)).toBe(label)
  })
})

describe('formatCountdown', () => {
  it.each([
    [3 * DAY + 5 * HOUR + 20 * MIN, '3d 5h'],
    [2 * HOUR + 17 * MIN + 8_000, '2h 17m'],
    [4 * MIN + 40_000, '4m 40s'],
    [4 * MIN + 5_000, '4m 05s'],
    [9_000, '9s'],
    [0, 'now'],
    [-5_000, 'now'],
  ])('%d ms ahead -> %s', (ahead, label) => {
    expect(formatCountdown(NOW + ahead, NOW)).toBe(label)
  })
  it('does not show 0s before the target is reached', () => {
    expect(formatCountdown(NOW + 400, NOW)).toBe('1s')
  })
  it('speaks the same value in words', () => {
    expect(countdownSpoken(NOW + 4 * MIN + 40_000, NOW)).toBe('4 minutes')
    expect(countdownSpoken(NOW + 2 * HOUR + 1 * MIN, NOW)).toBe('2 hours 1 minute')
  })
})

describe('percentUsed and meterTone', () => {
  it('prefers percentUsed and clamps it', () => {
    expect(percentUsed(snap({ percentUsed: 140 }))).toBe(100)
    expect(percentUsed(snap({ percentUsed: -3 }))).toBe(0)
  })
  it('derives from used/limit, then remaining/limit', () => {
    expect(percentUsed(snap({ percentUsed: undefined, unit: 'usd', used: 25, limit: 100 }))).toBe(25)
    expect(percentUsed(snap({ percentUsed: undefined, unit: 'usd', remaining: 80, limit: 100 }))).toBe(20)
  })
  it('is undefined when there is nothing to measure against', () => {
    expect(percentUsed(snap({ percentUsed: undefined, unit: 'usd', remaining: 8.9 }))).toBeUndefined()
  })
  it('classifies tone', () => {
    expect(meterTone(54)).toBe('calm')
    expect(meterTone(90)).toBe('high')
    expect(meterTone(100)).toBe('full')
    expect(meterTone(undefined)).toBe('calm')
  })
})

describe('alerts', () => {
  it('builds the stable alert id from the spec', () => {
    expect(alertIdFor(snap())).toBe('reset:claude:personal:session_5h')
  })
  it('creates an intent at the reset time, only for a future reset', () => {
    const s = snap()
    expect(makeAlertIntent(s)).toEqual({ id: 'reset:claude:personal:session_5h', fireAt: s.resetsAt, enabled: true })
    expect(makeAlertIntent(snap({ resetsAt: undefined }))).toBeUndefined()
    expect(canAlert(snap({ resetsAt: new Date(NOW - 1).toISOString() }), NOW)).toBe(false)
    expect(canAlert(s, NOW)).toBe(true)
  })
})

describe('ordering', () => {
  it('finds the soonest future reset', () => {
    const a = snap({ id: 'a', resetsAt: new Date(NOW + 5 * HOUR).toISOString() })
    const b = snap({ id: 'b', resetsAt: new Date(NOW + 2 * HOUR).toISOString() })
    const c = snap({ id: 'c', resetsAt: new Date(NOW - HOUR).toISOString() })
    expect(nextReset([a, b, c], NOW)?.id).toBe('b')
    expect(nextReset([c], NOW)).toBeUndefined()
  })
  it('sorts by provider order then window order', () => {
    const list = [
      snap({ id: '1', provider: 'xai', window: 'balance' }),
      snap({ id: '2', provider: 'claude', window: 'weekly' }),
      snap({ id: '3', provider: 'claude', window: 'session_5h' }),
      snap({ id: '4', provider: 'codex', window: 'session_5h' }),
    ]
    expect(sortSnapshots(list).map((s) => s.id)).toEqual(['3', '2', '4', '1'])
  })
})

describe('formatMoney', () => {
  it('formats dollars and yuan, and treats other units as dollars', () => {
    expect(formatMoney(12.345, 'usd')).toBe('$12.35')
    expect(formatMoney(110, 'cny')).toBe('CN¥110.00')
    expect(formatMoney(7, 'percent')).toBe('$7.00')
    expect(formatMoney(52.4, 'cny', 0)).toBe('CN¥52')
  })

  it('knows which units are money', () => {
    expect(isMoneyUnit('usd')).toBe(true)
    expect(isMoneyUnit('cny')).toBe(true)
    expect(isMoneyUnit('percent')).toBe(false)
  })
})

describe('providerVerdict', () => {
  const weekly = (over: Partial<Snapshot> = {}) => snap({ id: 'claude:personal:weekly', window: 'weekly', resetsAt: new Date(NOW + 3 * DAY).toISOString(), ...over })

  it('is ready when every window is below the warning line', () => {
    expect(providerVerdict([snap({ percentUsed: 54 }), weekly({ percentUsed: 38 })], NOW)).toEqual({ kind: 'ready' })
  })

  it('names the worst window when one runs low', () => {
    expect(providerVerdict([snap({ percentUsed: 40 }), weekly({ percentUsed: 91 })], NOW)).toEqual({ kind: 'low', window: 'weekly', percent: 91 })
  })

  it('is blocked until the last used-up window resets', () => {
    const v = providerVerdict([snap({ percentUsed: 100 }), weekly({ percentUsed: 100 })], NOW)
    expect(v).toEqual({ kind: 'blocked', until: NOW + 3 * DAY })
  })

  it('treats a used-up window whose reset passed as back', () => {
    expect(providerVerdict([snap({ percentUsed: 100, resetsAt: new Date(NOW - MIN).toISOString() })], NOW)).toEqual({ kind: 'ready' })
  })

  it('is unknown with no readings, and blocked with an unknown end when no reset time is known', () => {
    expect(providerVerdict([snap({ percentUsed: undefined })], NOW)).toEqual({ kind: 'unknown' })
    expect(providerVerdict([snap({ percentUsed: 100, resetsAt: undefined })], NOW)).toEqual({ kind: 'blocked', until: undefined })
  })
})

describe('groupForOverview', () => {
  it('groups limit windows per provider and account and keeps balances apart', () => {
    const balance = snap({ id: 'openrouter:personal:balance', provider: 'openrouter', window: 'balance', unit: 'usd', percentUsed: undefined, remaining: 3 })
    const { limits, balances } = groupForOverview([
      snap({ id: 'codex:personal:weekly', provider: 'codex', window: 'weekly' }),
      balance,
      snap(),
      snap({ id: 'claude:personal:weekly', window: 'weekly' }),
    ])
    expect(limits.map((g) => [g.key, g.windows.map((w) => w.window)])).toEqual([
      ['claude:personal', ['session_5h', 'weekly']],
      ['codex:personal', ['weekly']],
    ])
    expect(balances).toEqual([balance])
  })
})
describe('windowPace', () => {
  it('compares use with an even spend over the window', () => {
    // A 5-hour window resetting in 1 hour is 80% through; 50% used is 30 points in reserve.
    expect(windowPace(snap(), NOW)).toEqual({ elapsed: 80, delta: -30 })
    expect(windowPace(snap({ percentUsed: 95 }), NOW)).toEqual({ elapsed: 80, delta: 15 })
  })

  it('has nothing to say without a fixed window, a reset time or a reading, or once the reset passed', () => {
    expect(windowPace(snap({ window: 'balance' }), NOW)).toBeUndefined()
    expect(windowPace(snap({ resetsAt: undefined }), NOW)).toBeUndefined()
    expect(windowPace(snap({ percentUsed: undefined }), NOW)).toBeUndefined()
    expect(windowPace(snap({ resetsAt: new Date(NOW - MIN).toISOString() }), NOW)).toBeUndefined()
  })
})
