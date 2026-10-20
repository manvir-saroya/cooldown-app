// Runtime validators (zod 4) that mirror the frozen contract in ./types.ts exactly.
// Spec: docs/superpowers/specs/2026-10-10-cooldown-design.md section 4.
//
// Strictness rules beyond the bare shapes:
//   - unknown keys are rejected everywhere (z.strictObject);
//   - percentUsed is 0..100, used/limit are >= 0, remaining may be negative (overspent keys);
//   - timestamps are ISO-8601 UTC ("...Z", no offsets);
//   - id must equal `${provider}:${account}:${window}`;
//   - account is a short local label: never an email ('@'), never longer than 64 characters,
//     never contains ':' (ids are colon-joined) or control characters, never starts like a
//     well-known token;
//   - provider is a lowercase slug.
//
// Rules expressed with `.refine()` (id equality, token-prefix check, unique ids) have no JSON
// Schema equivalent, so the generated JSON Schema files are slightly looser than these validators.
// Everything else (keys, enums, ranges, patterns, lengths) is carried into the JSON Schema.

import { z } from 'zod';
import type { Snapshot, SyncPayload, AlertIntent } from './types';

export const SCHEMA_VERSION = 1 as const;

export const SourceKindSchema = z.enum(['official', 'estimated', 'scraped', 'manual']);
export const WindowKindSchema = z.enum(['session_5h', 'weekly', 'monthly', 'daily', 'balance']);
export const StatusSchema = z.enum(['ok', 'stale', 'degraded', 'unavailable']);
export const UnitSchema = z.enum(['percent', 'usd', 'cny', 'tokens', 'requests']);

/** ISO-8601 in UTC with a trailing "Z" (offsets such as +02:00 are rejected). */
export const IsoUtcSchema = z.iso.datetime();

const PROVIDER_PATTERN = /^[a-z0-9][a-z0-9_.-]{0,31}$/;
/** 1..64 characters, no '@', no ':', no control characters. */
const ACCOUNT_PATTERN = /^[^@:\u0000-\u001f\u007f]{1,64}$/;
/** Prefixes of well-known credential formats. A label that starts like one is refused. */
const TOKEN_PREFIX = /^(?:sk-|sk_|pk_|ghp_|gho_|ghu_|ghs_|github_pat_|xai-|xoxb-|xoxp-|bearer\s)|^eyJ/i;

export const ProviderSchema = z
  .string()
  .max(32)
  .regex(PROVIDER_PATTERN, 'provider must be a lowercase slug such as "claude" or "openrouter"');

export const AccountSchema = z
  .string()
  .min(1)
  .max(64, 'account must be at most 64 characters (it is a label, never a token)')
  .regex(ACCOUNT_PATTERN, 'account must not contain "@", ":" or control characters')
  .refine((value) => !TOKEN_PREFIX.test(value), 'account must not look like a token');

export const SnapshotSourceSchema = z.strictObject({
  kind: SourceKindSchema,
  adapter: z.string().min(1).max(64),
  adapterVersion: z.string().min(1).max(32),
});

export const SnapshotSchema = z
  .strictObject({
    schemaVersion: z.literal(SCHEMA_VERSION),
    id: z.string().min(1),
    provider: ProviderSchema,
    account: AccountSchema,
    window: WindowKindSchema,
    unit: UnitSchema,
    percentUsed: z.number().min(0).max(100).optional(),
    used: z.number().nonnegative().optional(),
    limit: z.number().nonnegative().optional(),
    remaining: z.number().optional(),
    resetsAt: IsoUtcSchema.optional(),
    observedAt: IsoUtcSchema,
    source: SnapshotSourceSchema,
    status: StatusSchema,
    note: z.string().max(500).optional(),
  })
  .refine((s) => s.id === `${s.provider}:${s.account}:${s.window}`, {
    path: ['id'],
    message: 'id must equal `${provider}:${account}:${window}`',
  });

/** `reset:${provider}:${account}:${window}` */
export const ALERT_ID_PATTERN =
  /^reset:[a-z0-9][a-z0-9_.-]{0,31}:[^@:\u0000-\u001f\u007f]{1,64}:(?:session_5h|weekly|monthly|daily|balance)$/;

export const AlertIntentSchema = z.strictObject({
  id: z.string().regex(ALERT_ID_PATTERN, 'alert id must be `reset:${provider}:${account}:${window}`'),
  fireAt: IsoUtcSchema,
  enabled: z.boolean(),
});

export const SyncPayloadSchema = z
  .strictObject({
    schemaVersion: z.literal(SCHEMA_VERSION),
    deviceId: z.string().min(1).max(128),
    generatedAt: IsoUtcSchema,
    snapshots: z.array(SnapshotSchema).max(256),
    alerts: z.array(AlertIntentSchema).max(256),
  })
  .superRefine((payload, ctx) => {
    const seenSnapshots = new Set<string>();
    payload.snapshots.forEach((snapshot, index) => {
      if (seenSnapshots.has(snapshot.id)) {
        ctx.addIssue({
          code: 'custom',
          path: ['snapshots', index, 'id'],
          message: `duplicate snapshot id ${snapshot.id}`,
        });
      }
      seenSnapshots.add(snapshot.id);
    });
    const seenAlerts = new Set<string>();
    payload.alerts.forEach((alert, index) => {
      if (seenAlerts.has(alert.id)) {
        ctx.addIssue({
          code: 'custom',
          path: ['alerts', index, 'id'],
          message: `duplicate alert id ${alert.id} (alerts are replaced, never stacked)`,
        });
      }
      seenAlerts.add(alert.id);
    });
  });

// --- Compile-time proof that the validators and the frozen types describe the same shapes. ---
type Mirrors<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;
type MustBeTrue<T extends true> = T;
type _SnapshotMirrorsType = MustBeTrue<Mirrors<z.infer<typeof SnapshotSchema>, Snapshot>>;
type _AlertIntentMirrorsType = MustBeTrue<Mirrors<z.infer<typeof AlertIntentSchema>, AlertIntent>>;
type _SyncPayloadMirrorsType = MustBeTrue<Mirrors<z.infer<typeof SyncPayloadSchema>, SyncPayload>>;

// --- Non-throwing parse API. ---

export type ParseResult<T> = { ok: true; value: T } | { ok: false; errors: string[] };

function describeIssues(error: z.ZodError): string[] {
  return error.issues.map((issue) => {
    const path = issue.path.map(String).join('.');
    return `${path === '' ? '(root)' : path}: ${issue.message}`;
  });
}

function parseWith<T>(schema: z.ZodType<T>, input: unknown): ParseResult<T> {
  try {
    const result = schema.safeParse(input);
    if (result.success) return { ok: true, value: result.data };
    return { ok: false, errors: describeIssues(result.error) };
  } catch {
    // Hostile inputs (throwing getters, proxies) must never escape as exceptions.
    return { ok: false, errors: ['(root): input could not be inspected'] };
  }
}

/** Validate one Snapshot. Never throws. */
export function parseSnapshot(input: unknown): ParseResult<Snapshot> {
  return parseWith<Snapshot>(SnapshotSchema, input);
}

/** Validate a SyncPayload (including every snapshot and alert in it). Never throws. */
export function parseSyncPayload(input: unknown): ParseResult<SyncPayload> {
  return parseWith<SyncPayload>(SyncPayloadSchema, input);
}
