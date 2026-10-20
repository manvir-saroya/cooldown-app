import { describe, expect, it } from 'vitest';
import {
  parseSnapshot,
  parseSyncPayload,
  type Snapshot,
  type SyncPayload,
} from '../src/index';

const snapshot: Snapshot = {
  schemaVersion: 1,
  id: 'claude:default:session_5h',
  provider: 'claude',
  account: 'default',
  window: 'session_5h',
  unit: 'percent',
  percentUsed: 42.5,
  resetsAt: '2026-10-10T14:37:00Z',
  observedAt: '2026-10-10T12:00:00Z',
  source: { kind: 'official', adapter: 'claude-statusline', adapterVersion: '0.1.0' },
  status: 'ok',
};

const payload: SyncPayload = {
  schemaVersion: 1,
  deviceId: 'device-0001',
  generatedAt: '2026-10-10T12:00:05Z',
  snapshots: [snapshot],
  alerts: [{ id: 'reset:claude:default:session_5h', fireAt: '2026-10-10T14:37:00Z', enabled: true }],
};

const bad = (overrides: Record<string, unknown>) => ({ ...snapshot, ...overrides });

describe('parseSnapshot', () => {
  it('accepts a valid snapshot and returns it unchanged', () => {
    const result = parseSnapshot(snapshot);
    expect(result).toEqual({ ok: true, value: snapshot });
  });

  it('accepts a snapshot with every optional field', () => {
    const full = bad({
      unit: 'usd',
      window: 'balance',
      id: 'claude:default:balance',
      used: 25.5,
      limit: 50,
      remaining: 24.5,
      note: 'No spending limit set on this key',
    });
    expect(parseSnapshot(full).ok).toBe(true);
  });

  it('accepts a negative remaining balance (overspent keys)', () => {
    expect(parseSnapshot(bad({ remaining: -0.35 })).ok).toBe(true);
  });

  it('rejects unknown keys at every level', () => {
    expect(parseSnapshot(bad({ extra: 1 })).ok).toBe(false);
    expect(parseSnapshot(bad({ source: { ...snapshot.source, extra: 1 } })).ok).toBe(false);
  });

  it('rejects null for optional fields and missing required fields', () => {
    expect(parseSnapshot(bad({ percentUsed: null })).ok).toBe(false);
    const { observedAt: _omitted, ...withoutObservedAt } = snapshot;
    expect(parseSnapshot(withoutObservedAt).ok).toBe(false);
  });

  it('enforces percentUsed 0..100 inclusive', () => {
    expect(parseSnapshot(bad({ percentUsed: 0 })).ok).toBe(true);
    expect(parseSnapshot(bad({ percentUsed: 100 })).ok).toBe(true);
    expect(parseSnapshot(bad({ percentUsed: -0.01 })).ok).toBe(false);
    expect(parseSnapshot(bad({ percentUsed: 100.01 })).ok).toBe(false);
    expect(parseSnapshot(bad({ percentUsed: Number.NaN })).ok).toBe(false);
    expect(parseSnapshot(bad({ percentUsed: Number.POSITIVE_INFINITY })).ok).toBe(false);
    expect(parseSnapshot(bad({ percentUsed: '42' })).ok).toBe(false);
  });

  it('rejects negative used and limit', () => {
    expect(parseSnapshot(bad({ used: -1 })).ok).toBe(false);
    expect(parseSnapshot(bad({ limit: -1 })).ok).toBe(false);
  });

  it('requires ISO-8601 UTC timestamps', () => {
    for (const field of ['observedAt', 'resetsAt'] as const) {
      expect(parseSnapshot(bad({ [field]: '2026-10-10T12:00:00Z' })).ok).toBe(true);
      expect(parseSnapshot(bad({ [field]: '2026-10-10T12:00:00.123Z' })).ok).toBe(true);
      expect(parseSnapshot(bad({ [field]: '2026-10-10T12:00:00+02:00' })).ok).toBe(false);
      expect(parseSnapshot(bad({ [field]: '2026-10-10T12:00:00' })).ok).toBe(false);
      expect(parseSnapshot(bad({ [field]: '2026-10-10' })).ok).toBe(false);
      expect(parseSnapshot(bad({ [field]: '2026-02-30T12:00:00Z' })).ok).toBe(false);
      expect(parseSnapshot(bad({ [field]: 1791633600 })).ok).toBe(false);
      expect(parseSnapshot(bad({ [field]: 'tomorrow' })).ok).toBe(false);
    }
  });

  it('requires id to equal provider:account:window', () => {
    const result = parseSnapshot(bad({ id: 'claude:default:weekly' }));
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.errors.join('\n')).toMatch(/^id: id must equal/m);
    expect(parseSnapshot(bad({ account: 'work', id: 'claude:work:session_5h' })).ok).toBe(true);
  });

  it('rejects emails, tokens and over-long values as account labels', () => {
    const withAccount = (account: string) => parseSnapshot(bad({ account, id: `claude:${account}:session_5h` }));
    expect(withAccount('work laptop').ok).toBe(true);
    expect(withAccount('a'.repeat(64)).ok).toBe(true);
    expect(withAccount('a'.repeat(65)).ok).toBe(false);
    expect(withAccount('').ok).toBe(false);
    expect(withAccount('me@example.com').ok).toBe(false);
    expect(withAccount('@handle').ok).toBe(false);
    expect(withAccount('team:prod').ok).toBe(false);
    expect(withAccount('line\nbreak').ok).toBe(false);
    expect(withAccount('sk-or-v1-abcdef').ok).toBe(false);
    expect(withAccount('SK-ANT-abcdef').ok).toBe(false);
    expect(withAccount('ghp_abcdef').ok).toBe(false);
    expect(withAccount('eyJhbGciOiJIUzI1NiJ9').ok).toBe(false);
    expect(withAccount('Bearer abc').ok).toBe(false);
    expect(withAccount('desk-key-2').ok).toBe(true);
  });

  it('rejects providers that are not lowercase slugs', () => {
    expect(parseSnapshot(bad({ provider: 'Claude', id: 'Claude:default:session_5h' })).ok).toBe(false);
    expect(parseSnapshot(bad({ provider: 'a:b', id: 'a:b:default:session_5h' })).ok).toBe(false);
    expect(parseSnapshot(bad({ provider: 'gemini-cli', id: 'gemini-cli:default:session_5h' })).ok).toBe(true);
  });

  it('rejects unknown enum values and a wrong schemaVersion', () => {
    expect(parseSnapshot(bad({ status: 'fine' })).ok).toBe(false);
    expect(parseSnapshot(bad({ window: 'hourly' })).ok).toBe(false);
    expect(parseSnapshot(bad({ unit: 'eur' })).ok).toBe(false);
    expect(parseSnapshot(bad({ schemaVersion: 2 })).ok).toBe(false);
    expect(parseSnapshot(bad({ source: { ...snapshot.source, kind: 'guessed' } })).ok).toBe(false);
  });

  it('bounds the note length', () => {
    expect(parseSnapshot(bad({ note: 'x'.repeat(500) })).ok).toBe(true);
    expect(parseSnapshot(bad({ note: 'x'.repeat(501) })).ok).toBe(false);
  });

  it('reports errors as "path: message" strings', () => {
    const result = parseSnapshot(bad({ percentUsed: 101, source: { kind: 'official' } }));
    expect(result.ok).toBe(false);
    if (!result.ok) {
      expect(result.errors.some((e) => e.startsWith('percentUsed:'))).toBe(true);
      expect(result.errors.some((e) => e.startsWith('source.adapter:'))).toBe(true);
    }
  });

  it('never throws, whatever it is given', () => {
    const circular: Record<string, unknown> = {};
    circular.self = circular;
    const hostile = new Proxy({}, {
      get() {
        throw new Error('boom');
      },
      ownKeys() {
        throw new Error('boom');
      },
    });
    const inputs: unknown[] = [undefined, null, 0, 42, '', 'text', true, [], [snapshot], {}, circular, hostile, Symbol('x'), () => 1];
    for (const input of inputs) {
      expect(() => parseSnapshot(input)).not.toThrow();
      expect(parseSnapshot(input).ok).toBe(false);
      expect(() => parseSyncPayload(input)).not.toThrow();
      expect(parseSyncPayload(input).ok).toBe(false);
    }
  });
});

describe('parseSyncPayload', () => {
  it('accepts a valid payload', () => {
    expect(parseSyncPayload(payload)).toEqual({ ok: true, value: payload });
  });

  it('accepts empty snapshot and alert lists', () => {
    expect(parseSyncPayload({ ...payload, snapshots: [], alerts: [] }).ok).toBe(true);
  });

  it('rejects unknown keys', () => {
    expect(parseSyncPayload({ ...payload, token: 'x' }).ok).toBe(false);
    expect(parseSyncPayload({ ...payload, alerts: [{ ...payload.alerts[0], extra: true }] }).ok).toBe(false);
  });

  it('validates nested snapshots', () => {
    const result = parseSyncPayload({ ...payload, snapshots: [{ ...snapshot, percentUsed: 500 }] });
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.errors[0]).toMatch(/^snapshots\.0\.percentUsed:/);
  });

  it('validates alert ids and times', () => {
    const alert = payload.alerts[0]!;
    expect(parseSyncPayload({ ...payload, alerts: [{ ...alert, id: 'claude:default:session_5h' }] }).ok).toBe(false);
    expect(parseSyncPayload({ ...payload, alerts: [{ ...alert, id: 'reset:claude:default:hourly' }] }).ok).toBe(false);
    expect(parseSyncPayload({ ...payload, alerts: [{ ...alert, id: 'reset:claude:me@x.io:weekly' }] }).ok).toBe(false);
    expect(parseSyncPayload({ ...payload, alerts: [{ ...alert, fireAt: 'soon' }] }).ok).toBe(false);
    expect(parseSyncPayload({ ...payload, alerts: [{ ...alert, enabled: 'yes' }] }).ok).toBe(false);
    expect(parseSyncPayload({ ...payload, alerts: [{ ...alert, id: 'reset:codex:work laptop:weekly' }] }).ok).toBe(true);
  });

  it('rejects duplicate snapshot ids and duplicate alert ids', () => {
    expect(parseSyncPayload({ ...payload, snapshots: [snapshot, snapshot] }).ok).toBe(false);
    expect(parseSyncPayload({ ...payload, alerts: [payload.alerts[0], payload.alerts[0]] }).ok).toBe(false);
  });

  it('requires a device id and the right schemaVersion', () => {
    expect(parseSyncPayload({ ...payload, deviceId: '' }).ok).toBe(false);
    expect(parseSyncPayload({ ...payload, deviceId: 'd'.repeat(129) }).ok).toBe(false);
    expect(parseSyncPayload({ ...payload, schemaVersion: 2 }).ok).toBe(false);
  });

  it('caps the number of snapshots and alerts', () => {
    const many = Array.from({ length: 257 }, (_, i) => ({ ...snapshot, account: `a${i}`, id: `claude:a${i}:session_5h` }));
    expect(parseSyncPayload({ ...payload, snapshots: many }).ok).toBe(false);
  });
});
