// The adapter table and the fixture-context contract.
// See fixtures/README.md for how Rust and TypeScript tests consume these.

import { z } from 'zod';
import { AccountSchema, IsoUtcSchema, SnapshotSchema } from './schema';
import type { SourceKind, WindowKind } from './types';

export interface AdapterInfo {
  /** Value of `source.adapter` and the directory name under fixtures/adapters/. */
  id: string;
  provider: string;
  /** Value of `source.kind` for every snapshot the adapter emits. */
  sourceKind: SourceKind;
  /**
   * The windows the adapter emits on every poll, in this order. A window with no usable data is
   * still emitted (status 'unavailable' or a carried-forward 'degraded'/'stale' value).
   */
  windows: readonly WindowKind[];
}

export const ADAPTERS = [
  { id: 'claude-statusline', provider: 'claude', sourceKind: 'official', windows: ['session_5h', 'weekly'] },
  { id: 'codex-app-server', provider: 'codex', sourceKind: 'scraped', windows: ['session_5h', 'weekly'] },
  { id: 'codex-rollout-log', provider: 'codex', sourceKind: 'scraped', windows: ['session_5h', 'weekly'] },
  { id: 'openrouter', provider: 'openrouter', sourceKind: 'official', windows: ['balance'] },
  { id: 'deepseek', provider: 'deepseek', sourceKind: 'official', windows: ['balance'] },
  { id: 'xai', provider: 'xai', sourceKind: 'official', windows: ['balance'] },
  { id: 'kimi', provider: 'kimi', sourceKind: 'official', windows: ['balance'] },
  { id: 'copilot', provider: 'copilot', sourceKind: 'official', windows: ['monthly'] },
  { id: 'antigravity-statusline', provider: 'antigravity', sourceKind: 'official', windows: ['weekly'] },
] as const satisfies readonly AdapterInfo[];

export type AdapterId = (typeof ADAPTERS)[number]['id'];

export const ADAPTER_IDS = ADAPTERS.map((adapter) => adapter.id) as readonly AdapterId[];

/**
 * `<case>.context.json`: everything an adapter needs besides the raw payload, so a fixture run is
 * a pure function `(input, context) -> Snapshot[]`.
 */
export const FixtureContextSchema = z.strictObject({
  /** The fixed "current time" of the poll. Always ISO-8601 UTC. */
  now: IsoUtcSchema,
  /** Local account label the adapter was configured with. */
  account: AccountSchema,
  adapterVersion: z.string().min(1).max(32),
  /** When the source produced the payload, if that differs from `now` (stale inbox files). */
  observedAt: IsoUtcSchema.optional(),
  /** The adapter's last emitted snapshots, used to carry values forward or detect resets. */
  previous: z.array(SnapshotSchema).optional(),
  /** HTTP status of the response, for the API-key adapters. Absent for non-HTTP sources. */
  httpStatus: z.number().int().min(100).max(599).optional(),
});

export type FixtureContext = z.infer<typeof FixtureContextSchema>;
