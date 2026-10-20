import { readFileSync } from 'node:fs';
import { basename, join } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  ADAPTERS,
  AlertIntentSchema,
  FixtureContextSchema,
  alertIdFor,
  effectiveStatus,
  parseSnapshot,
  snapshotIdFor,
  type Snapshot,
} from '../src/index';
import { REFERENCE_ADAPTERS } from './reference-adapters';
import { FIXTURES_DIR, adapterDirs, allCases, allFixtureFiles, caseNames } from './load-fixtures';

const cases = allCases();

describe('fixture layout', () => {
  it('has a directory for every adapter and nothing else', () => {
    expect(adapterDirs()).toEqual(ADAPTERS.map((adapter) => adapter.id).sort());
  });

  it('has at least the required cases per adapter', () => {
    const required: Record<string, string[]> = {
      'claude-statusline': ['both-windows', 'only-five-hour', 'no-rate-limits', 'window-reset-missing', 'malformed-fields', 'null-fields'],
      'codex-app-server': ['both-windows', 'weekly-in-primary'],
      'codex-rollout-log': ['both-windows'],
      openrouter: ['capped-key'],
      deepseek: ['usd-and-cny'],
      xai: ['prepaid-balance'],
    };
    for (const [adapter, names] of Object.entries(required)) {
      expect(caseNames(adapter), adapter).toEqual(expect.arrayContaining(names));
    }
  });

  it('has input, context and expected for every case, and no stray files', () => {
    for (const file of allFixtureFiles()) {
      expect(basename(file), file).toMatch(/^[a-z0-9-]+\.(input|context|expected)\.json$/);
    }
    for (const adapter of adapterDirs()) {
      for (const name of caseNames(adapter)) {
        for (const kind of ['input', 'context', 'expected']) {
          expect(allFixtureFiles(), `${adapter}/${name}.${kind}.json`).toContain(join(FIXTURES_DIR, adapter, `${name}.${kind}.json`));
        }
      }
    }
  });
});

describe('fixture data is obviously fake', () => {
  const EMAIL = /[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+\.[A-Za-z]{2,}/;
  const SECRET = /\b(?:sk-[A-Za-z0-9_-]{8,}|ghp_[A-Za-z0-9]{8,}|xai-[A-Za-z0-9]{8,}|eyJ[A-Za-z0-9_-]{10,}|Bearer\s+\S+)/;
  for (const file of allFixtureFiles()) {
    it(basename(file), () => {
      const text = readFileSync(file, 'utf8');
      expect(text).not.toMatch(EMAIL);
      expect(text).not.toMatch(SECRET);
      expect(() => JSON.parse(text)).not.toThrow();
      expect(text.endsWith('\n')).toBe(true);
    });
  }
});

describe.each(cases.map((c) => [`${c.adapter}/${c.name}`, c] as const))('%s', (_label, fixture) => {
  const adapter = ADAPTERS.find((entry) => entry.id === fixture.adapter)!;
  const { context, expected } = fixture;
  const nowMs = Date.parse(context.now);

  it('has a valid context', () => {
    const parsed = FixtureContextSchema.safeParse(context);
    expect(parsed.success, JSON.stringify(parsed.error?.issues)).toBe(true);
    expect(context.adapterVersion).toBe('0.1.0');
  });

  it('expected is a Snapshot[] that validates against the zod schema', () => {
    expect(Array.isArray(expected)).toBe(true);
    for (const snapshot of expected) {
      const parsed = parseSnapshot(snapshot);
      expect(parsed.ok, parsed.ok ? '' : parsed.errors.join('; ')).toBe(true);
    }
    for (const previous of context.previous ?? []) {
      const parsed = parseSnapshot(previous);
      expect(parsed.ok, parsed.ok ? '' : parsed.errors.join('; ')).toBe(true);
    }
  });

  it('emits exactly the adapter\'s windows, in order, with consistent identity', () => {
    expect(expected.map((s) => s.window)).toEqual([...adapter.windows]);
    for (const s of expected) {
      expect(s.provider).toBe(adapter.provider);
      expect(s.account).toBe(context.account);
      expect(s.source).toEqual({ kind: adapter.sourceKind, adapter: adapter.id, adapterVersion: context.adapterVersion });
      expect(s.id).toBe(`${adapter.provider}:${context.account}:${s.window}`);
      expect(snapshotIdFor(s)).toBe(s.id);
      const alert = { id: alertIdFor(s), fireAt: s.resetsAt ?? context.now, enabled: true };
      expect(AlertIntentSchema.safeParse(alert).success, alert.id).toBe(true);
    }
  });

  it('follows the status and note rules', () => {
    for (const s of expected) {
      const label = `${s.id} (${s.status})`;
      // Never observed in the future.
      expect(Date.parse(s.observedAt), label).toBeLessThanOrEqual(nowMs);
      if (s.status === 'degraded' || s.status === 'unavailable' || s.status === 'stale') {
        expect(s.note, `${label} needs a note`).toBeTruthy();
      }
      if (s.note === 'window reset') {
        expect(s.status).toBe('ok');
        expect(s.percentUsed).toBe(0);
        expect(s.resetsAt).toBeUndefined();
        expect(s.observedAt).toBe(context.now);
      }
      if (s.status === 'ok' || s.status === 'stale' || s.status === 'degraded') {
        const hasValue = s.percentUsed !== undefined || s.used !== undefined || s.remaining !== undefined;
        expect(hasValue, `${label} needs a value`).toBe(true);
      }
      if (s.status === 'unavailable') {
        expect(s.percentUsed ?? s.used ?? s.limit ?? s.remaining, label).toBeUndefined();
      }
      if (s.status === 'ok') {
        expect(s.resetsAt === undefined || Date.parse(s.resetsAt) > nowMs, `${label} ok but reset passed`).toBe(true);
      }
      if (s.status === 'stale') {
        expect(s.resetsAt !== undefined && Date.parse(s.resetsAt) <= nowMs, `${label} stale without a past reset`).toBe(true);
      }
      // What a consumer derives with the age check disabled must match what the adapter stored.
      expect(effectiveStatus(s, new Date(context.now), { maxAgeMs: Infinity }), label).toBe(s.status);
    }
  });

  it('uses seconds-precision UTC timestamps', () => {
    for (const s of expected) {
      for (const value of [s.observedAt, s.resetsAt]) {
        if (value !== undefined) expect(value).toMatch(/^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/);
      }
    }
  });

  it('is what the reference adapter produces from the raw input and context', () => {
    const produced: Snapshot[] = REFERENCE_ADAPTERS[adapter.id](fixture.input, context);
    expect(produced).toEqual(expected);
  });
});

describe('semantic rules called out in fixtures/README.md', () => {
  const find = (adapter: string, name: string) => cases.find((c) => c.adapter === adapter && c.name === name)!;

  it('a missing Claude window after its reset time is a reset, not unknown', () => {
    const c = find('claude-statusline', 'window-reset-missing');
    const [session] = c.expected;
    expect(session).toMatchObject({ window: 'session_5h', percentUsed: 0, status: 'ok', note: 'window reset' });
    expect(session!.resetsAt).toBeUndefined();
  });

  it('Codex assigns windows by duration even when weekly arrives in the primary slot', () => {
    const c = find('codex-app-server', 'weekly-in-primary');
    const result = (c.input as { result: { rateLimits: { primary: { windowDurationMins: number }; secondary: unknown } } }).result;
    expect(result.rateLimits.primary.windowDurationMins).toBe(10080);
    expect(result.rateLimits.secondary).toBeNull();
    expect(c.expected.find((s) => s.window === 'weekly')).toMatchObject({ status: 'ok', percentUsed: 22 });
    expect(c.expected.find((s) => s.window === 'session_5h')?.status).toBe('unavailable');
  });

  it('source.kind is official for the statusline and API endpoints, scraped for the Codex paths', () => {
    for (const c of cases) {
      const expectedKind = c.adapter.startsWith('codex-') ? 'scraped' : 'official';
      for (const s of c.expected) expect(s.source.kind, `${c.adapter}/${c.name}`).toBe(expectedKind);
    }
  });

  it('the rollout log reports the event time, not the poll time', () => {
    const c = find('codex-rollout-log', 'both-windows');
    expect(c.expected[0]!.observedAt).toBe('2026-10-10T11:58:30Z');
    expect(c.expected[0]!.observedAt).not.toBe(c.context.now);
  });

  it('reference adapters never mutate their inputs', () => {
    for (const c of cases) {
      const input = structuredClone(c.input);
      const context = structuredClone(c.context);
      REFERENCE_ADAPTERS[c.adapter as keyof typeof REFERENCE_ADAPTERS](input, context);
      expect(input).toEqual(c.input);
      expect(context).toEqual(c.context);
    }
  });
});
