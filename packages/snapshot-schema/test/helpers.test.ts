import { afterEach, describe, expect, it } from 'vitest';
import {
  DEFAULT_MAX_AGE_MS,
  alertIdFor,
  effectiveStatus,
  formatAge,
  secondsUntilReset,
  snapshotIdFor,
  windowFromDurationMinutes,
  type Snapshot,
} from '../src/index';

const NOW = new Date('2026-10-10T12:00:00Z');
const MIN = 60_000;

function snap(overrides: Partial<Snapshot> = {}): Snapshot {
  return {
    schemaVersion: 1,
    id: 'claude:default:session_5h',
    provider: 'claude',
    account: 'default',
    window: 'session_5h',
    unit: 'percent',
    percentUsed: 42,
    resetsAt: '2026-10-10T14:00:00Z',
    observedAt: '2026-10-10T11:59:00Z',
    source: { kind: 'official', adapter: 'claude-statusline', adapterVersion: '0.1.0' },
    status: 'ok',
    ...overrides,
  };
}

describe('effectiveStatus', () => {
  it('keeps a fresh snapshot with a future reset as ok', () => {
    expect(effectiveStatus(snap(), NOW)).toBe('ok');
  });

  it('is stale when resetsAt is in the past, and exactly at now', () => {
    expect(effectiveStatus(snap({ resetsAt: '2026-10-10T11:59:59Z' }), NOW)).toBe('stale');
    expect(effectiveStatus(snap({ resetsAt: '2026-10-10T12:00:00Z' }), NOW)).toBe('stale');
    expect(effectiveStatus(snap({ resetsAt: '2026-10-10T12:00:01Z' }), NOW)).toBe('ok');
  });

  it('is stale when observedAt is older than the default max age (30 min), not before', () => {
    const exactly = new Date(NOW.getTime() - DEFAULT_MAX_AGE_MS).toISOString();
    const over = new Date(NOW.getTime() - DEFAULT_MAX_AGE_MS - 1000).toISOString();
    expect(effectiveStatus(snap({ observedAt: exactly }), NOW)).toBe('ok');
    expect(effectiveStatus(snap({ observedAt: over }), NOW)).toBe('stale');
  });

  it('honours a configurable max age', () => {
    const observedAt = '2026-10-10T11:50:00Z'; // 10 minutes old
    expect(effectiveStatus(snap({ observedAt }), NOW, { maxAgeMs: 5 * MIN })).toBe('stale');
    expect(effectiveStatus(snap({ observedAt }), NOW, { maxAgeMs: 15 * MIN })).toBe('ok');
    expect(effectiveStatus(snap({ observedAt: '2020-01-01T00:00:00Z' }), NOW, { maxAgeMs: Infinity })).toBe('ok');
  });

  it('falls back to the default max age for invalid options', () => {
    const old = snap({ observedAt: '2026-10-10T11:00:00Z' }); // 60 minutes old
    for (const maxAgeMs of [Number.NaN, 0, -5]) {
      expect(effectiveStatus(old, NOW, { maxAgeMs })).toBe('stale');
    }
  });

  it('never upgrades degraded or unavailable', () => {
    for (const status of ['degraded', 'unavailable'] as const) {
      expect(effectiveStatus(snap({ status }), NOW)).toBe(status);
      expect(effectiveStatus(snap({ status, resetsAt: '2026-10-10T11:00:00Z' }), NOW)).toBe(status);
      expect(effectiveStatus(snap({ status, observedAt: '2026-10-01T00:00:00Z' }), NOW)).toBe(status);
    }
  });

  it('never upgrades a stored stale snapshot back to ok', () => {
    expect(effectiveStatus(snap({ status: 'stale' }), NOW)).toBe('stale');
  });

  it('treats a snapshot without resetsAt (balances, window-reset markers) by age only', () => {
    const balance = snap({ resetsAt: undefined, window: 'balance', unit: 'usd' });
    expect(effectiveStatus(balance, NOW)).toBe('ok');
    expect(effectiveStatus({ ...balance, observedAt: '2026-10-10T10:00:00Z' }, NOW)).toBe('stale');
  });

  it('treats a future observedAt (clock skew) as fresh', () => {
    expect(effectiveStatus(snap({ observedAt: '2026-10-10T12:10:00Z' }), NOW)).toBe('ok');
  });

  it('treats an unparseable observedAt as stale and ignores an unparseable resetsAt', () => {
    expect(effectiveStatus(snap({ observedAt: 'garbage' }), NOW)).toBe('stale');
    expect(effectiveStatus(snap({ resetsAt: 'garbage' }), NOW)).toBe('ok');
  });

  it('uses the supplied clock, never the real one', () => {
    const s = snap();
    expect(effectiveStatus(s, new Date('2026-10-10T11:59:30Z'))).toBe('ok');
    expect(effectiveStatus(s, new Date('2030-01-01T00:00:00Z'))).toBe('stale');
  });
});

describe('ids', () => {
  it('builds snapshot and alert ids', () => {
    const s = snap({ provider: 'codex', account: 'work', window: 'weekly' });
    expect(snapshotIdFor(s)).toBe('codex:work:weekly');
    expect(alertIdFor(s)).toBe('reset:codex:work:weekly');
  });

  it('accepts a bare parts object', () => {
    expect(alertIdFor({ provider: 'openrouter', account: 'default', window: 'balance' })).toBe(
      'reset:openrouter:default:balance',
    );
  });
});

describe('windowFromDurationMinutes', () => {
  it('maps the nominal durations', () => {
    expect(windowFromDurationMinutes(300)).toBe('session_5h');
    expect(windowFromDurationMinutes(10080)).toBe('weekly');
    expect(windowFromDurationMinutes(43200)).toBe('monthly');
    expect(windowFromDurationMinutes(1440)).toBe('daily');
  });

  it('tolerates small deviations', () => {
    expect(windowFromDurationMinutes(299)).toBe('session_5h');
    expect(windowFromDurationMinutes(301)).toBe('session_5h');
    expect(windowFromDurationMinutes(10079)).toBe('weekly');
    expect(windowFromDurationMinutes(10081)).toBe('weekly');
    expect(windowFromDurationMinutes(28 * 1440)).toBe('monthly'); // 28-day month
    expect(windowFromDurationMinutes(31 * 1440)).toBe('monthly'); // 31-day month
  });

  it('decides by duration regardless of where a provider put the window', () => {
    // The weekly window arriving in a "primary" slot is still weekly.
    expect(windowFromDurationMinutes(10080)).toBe('weekly');
  });

  it('rejects durations that match nothing', () => {
    for (const minutes of [60, 240, 600, 2880, 5000, 20000, 100000]) {
      expect(windowFromDurationMinutes(minutes)).toBeUndefined();
    }
  });

  it('rejects invalid input without throwing', () => {
    for (const minutes of [0, -300, Number.NaN, Number.POSITIVE_INFINITY, '300' as unknown as number, undefined as unknown as number, null as unknown as number]) {
      expect(windowFromDurationMinutes(minutes)).toBeUndefined();
    }
  });

  it('honours a custom tolerance and never returns balance', () => {
    expect(windowFromDurationMinutes(330, 0.05)).toBeUndefined();
    expect(windowFromDurationMinutes(330, 0.11)).toBe('session_5h');
    expect(windowFromDurationMinutes(300, 0)).toBe('session_5h');
  });
});

describe('secondsUntilReset', () => {
  it('counts whole seconds until resetsAt', () => {
    expect(secondsUntilReset(snap({ resetsAt: '2026-10-10T14:00:00Z' }), NOW)).toBe(7200);
    expect(secondsUntilReset(snap({ resetsAt: '2026-10-10T12:00:01Z' }), NOW)).toBe(1);
  });

  it('rounds partial seconds up', () => {
    expect(secondsUntilReset(snap({ resetsAt: '2026-10-10T12:00:01Z' }), new Date('2026-10-10T12:00:00.400Z'))).toBe(1);
    expect(secondsUntilReset(snap({ resetsAt: '2026-10-10T12:00:01Z' }), new Date('2026-10-10T12:00:00.999Z'))).toBe(1);
  });

  it('is 0 at and after the reset, never negative', () => {
    expect(secondsUntilReset(snap({ resetsAt: '2026-10-10T12:00:00Z' }), NOW)).toBe(0);
    expect(secondsUntilReset(snap({ resetsAt: '2026-10-09T12:00:00Z' }), NOW)).toBe(0);
  });

  it('is undefined without a usable resetsAt', () => {
    expect(secondsUntilReset(snap({ resetsAt: undefined }), NOW)).toBeUndefined();
    expect(secondsUntilReset(snap({ resetsAt: 'garbage' }), NOW)).toBeUndefined();
  });

  it('handles fractional-second reset times', () => {
    expect(secondsUntilReset(snap({ resetsAt: '2026-10-10T12:00:30.500Z' }), NOW)).toBe(31);
  });
});

describe('formatAge', () => {
  const at = (secondsAgo: number) => new Date(NOW.getTime() - secondsAgo * 1000).toISOString();

  it('formats ages compactly', () => {
    expect(formatAge(at(0), NOW)).toBe('just now');
    expect(formatAge(at(59), NOW)).toBe('just now');
    expect(formatAge(at(60), NOW)).toBe('1 min ago');
    expect(formatAge(at(119), NOW)).toBe('1 min ago');
    expect(formatAge(at(120), NOW)).toBe('2 min ago');
    expect(formatAge(at(59 * 60 + 59), NOW)).toBe('59 min ago');
    expect(formatAge(at(3600), NOW)).toBe('1 h ago');
    expect(formatAge(at(5 * 3600 + 1800), NOW)).toBe('5 h ago');
    expect(formatAge(at(23 * 3600 + 3599), NOW)).toBe('23 h ago');
    expect(formatAge(at(24 * 3600), NOW)).toBe('1 d ago');
    expect(formatAge(at(3 * 86400 + 5), NOW)).toBe('3 d ago');
  });

  it('treats a future timestamp as just now', () => {
    expect(formatAge('2026-10-10T12:30:00Z', NOW)).toBe('just now');
  });

  it('accepts a Date and reports unknown for garbage', () => {
    expect(formatAge(new Date('2026-10-10T11:58:00Z'), NOW)).toBe('2 min ago');
    expect(formatAge('garbage', NOW)).toBe('unknown');
    expect(formatAge('2026-10-10T11:58:00Z', new Date(Number.NaN))).toBe('unknown');
  });
});

describe('time zone and DST independence', () => {
  const originalTz = process.env.TZ;
  afterEach(() => {
    if (originalTz === undefined) delete process.env.TZ;
    else process.env.TZ = originalTz;
  });

  // Instants straddling real DST transitions in each zone. `offset` is Date#getTimezoneOffset()
  // at `now`, which proves the zone really was applied.
  const cases = [
    { tz: 'UTC', now: '2026-03-08T06:30:00Z', resetsAt: '2026-03-08T08:30:00Z', offset: 0 },
    { tz: 'America/New_York', now: '2026-03-08T06:30:00Z', resetsAt: '2026-03-08T08:30:00Z', offset: 300 }, // spring forward at 07:00Z
    { tz: 'America/New_York', now: '2026-11-01T04:30:00Z', resetsAt: '2026-11-01T06:30:00Z', offset: 240 }, // fall back at 06:00Z
    { tz: 'Europe/London', now: '2026-03-29T00:30:00Z', resetsAt: '2026-03-29T02:30:00Z', offset: 0 }, // spring forward at 01:00Z
    { tz: 'Australia/Lord_Howe', now: '2026-10-03T14:00:00Z', resetsAt: '2026-10-03T16:00:00Z', offset: -630 }, // 30-minute DST shift
    { tz: 'Asia/Kolkata', now: '2026-10-10T12:00:00Z', resetsAt: '2026-10-10T14:00:00Z', offset: -330 }, // +05:30, no DST
  ];

  for (const { tz, now, resetsAt, offset } of cases) {
    it(`gives identical results in ${tz} around ${now}`, () => {
      process.env.TZ = tz;
      const nowDate = new Date(now);
      expect(nowDate.getTimezoneOffset()).toBe(offset);
      const s = snap({ resetsAt, observedAt: new Date(nowDate.getTime() - 5 * MIN).toISOString() });
      expect(secondsUntilReset(s, nowDate)).toBe(7200); // elapsed seconds, not wall-clock hours
      expect(effectiveStatus(s, nowDate)).toBe('ok');
      expect(effectiveStatus(s, new Date(new Date(resetsAt).getTime() + 1000))).toBe('stale');
      expect(formatAge(s.observedAt, nowDate)).toBe('5 min ago');
      expect(formatAge(new Date(nowDate.getTime() - 26 * 3600 * 1000), nowDate)).toBe('1 d ago');
    });
  }
});
