import { describe, expect, it } from 'vitest';
import { z } from 'zod';
import { SnapshotSchema, SyncPayloadSchema, parseSyncPayload, type Snapshot, type SyncPayload } from '../src/index';
import { PACKAGE_ROOT, allCases, readJson } from './load-fixtures';
import { join } from 'node:path';

const SNAPSHOT_FILE = join(PACKAGE_ROOT, 'schema', 'snapshot.schema.json');
const SYNC_FILE = join(PACKAGE_ROOT, 'schema', 'sync-payload.schema.json');

type JsonSchemaInput = Parameters<typeof z.fromJSONSchema>[0];
const fromFile = (path: string) => z.fromJSONSchema(readJson(path) as JsonSchemaInput);

const snapshotValidator = fromFile(SNAPSHOT_FILE);
const syncValidator = fromFile(SYNC_FILE);

const withoutTitle = (schema: unknown) => {
  const { title: _title, ...rest } = schema as Record<string, unknown>;
  return rest;
};

describe('generated JSON Schema files', () => {
  it('are up to date with the zod schemas (run `npm run build:schema` if this fails)', () => {
    expect(withoutTitle(readJson(SNAPSHOT_FILE))).toEqual(z.toJSONSchema(SnapshotSchema, { target: 'draft-2020-12' }));
    expect(withoutTitle(readJson(SYNC_FILE))).toEqual(z.toJSONSchema(SyncPayloadSchema, { target: 'draft-2020-12' }));
  });

  it('declare draft 2020-12 and forbid additional properties', () => {
    for (const file of [SNAPSHOT_FILE, SYNC_FILE]) {
      const schema = readJson(file) as { $schema: string; additionalProperties: boolean; type: string };
      expect(schema.$schema).toBe('https://json-schema.org/draft/2020-12/schema');
      expect(schema.type).toBe('object');
      expect(schema.additionalProperties).toBe(false);
    }
  });

  it('accept every expected fixture', () => {
    const cases = allCases();
    expect(cases.length).toBeGreaterThan(0);
    for (const c of cases) {
      for (const snapshot of c.expected) {
        const result = snapshotValidator.safeParse(snapshot);
        expect(result.success, `${c.adapter}/${c.name}: ${JSON.stringify(result.error?.issues)}`).toBe(true);
      }
    }
  });

  it('accept a sync payload built from every expected fixture', () => {
    const snapshots: Snapshot[] = [];
    const seen = new Set<string>();
    for (const c of allCases()) {
      for (const s of c.expected) {
        if (!seen.has(s.id)) {
          seen.add(s.id);
          snapshots.push(s);
        }
      }
    }
    const payload: SyncPayload = {
      schemaVersion: 1,
      deviceId: 'test-device',
      generatedAt: '2026-10-10T12:00:00Z',
      snapshots,
      alerts: [{ id: 'reset:claude:default:session_5h', fireAt: '2026-10-10T14:37:00Z', enabled: true }],
    };
    expect(parseSyncPayload(payload).ok).toBe(true);
    expect(syncValidator.safeParse(payload).success).toBe(true);
  });

  describe('reject what the zod schema rejects (for the rules JSON Schema can express)', () => {
    const base = allCases().find((c) => c.adapter === 'claude-statusline' && c.name === 'both-windows')!.expected[0]!;
    const bad: Array<[string, Record<string, unknown>]> = [
      ['unknown key', { ...base, extra: 1 }],
      ['unknown source key', { ...base, source: { ...base.source, extra: 1 } }],
      ['percentUsed above 100', { ...base, percentUsed: 100.5 }],
      ['percentUsed below 0', { ...base, percentUsed: -1 }],
      ['null percentUsed', { ...base, percentUsed: null }],
      ['offset timestamp', { ...base, observedAt: '2026-10-10T12:00:00+02:00' }],
      ['impossible date', { ...base, observedAt: '2026-02-30T12:00:00Z' }],
      ['email account', { ...base, account: 'me@example.com' }],
      ['long account', { ...base, account: 'a'.repeat(65) }],
      ['colon in account', { ...base, account: 'a:b' }],
      ['uppercase provider', { ...base, provider: 'Claude' }],
      ['bad status', { ...base, status: 'fine' }],
      ['bad window', { ...base, window: 'hourly' }],
      ['wrong schemaVersion', { ...base, schemaVersion: 2 }],
      ['missing observedAt', (({ observedAt: _o, ...rest }) => rest)(base)],
    ];
    for (const [label, value] of bad) {
      it(label, () => {
        expect(snapshotValidator.safeParse(value).success).toBe(false);
      });
    }
  });

  it('documents the rules that only the zod validators enforce', () => {
    // id equality and the token-prefix check are refinements: the JSON Schema is intentionally looser.
    const base = allCases()[0]!.expected[0]!;
    expect(snapshotValidator.safeParse({ ...base, id: 'claude:default:weekly' }).success).toBe(true);
    expect(SnapshotSchema.safeParse({ ...base, id: 'claude:default:weekly' }).success).toBe(false);
  });
});
