// Emits the JSON Schema files that the Rust adapter/sync tests validate against.
// Run with: npm run build:schema -w @cooldown/snapshot-schema
// Uses Node's built-in TypeScript type stripping, so there is no extra tooling.

import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { z } from 'zod';
import { SnapshotSchema, SyncPayloadSchema } from '../src/schema.ts';

const outDir = join(dirname(fileURLToPath(import.meta.url)), '..', 'schema');

export const OUTPUTS = [
  { file: 'snapshot.schema.json', schema: SnapshotSchema, title: 'Cooldown Snapshot' },
  { file: 'sync-payload.schema.json', schema: SyncPayloadSchema, title: 'Cooldown SyncPayload' },
] as const;

mkdirSync(outDir, { recursive: true });
for (const { file, schema, title } of OUTPUTS) {
  const jsonSchema = { ...z.toJSONSchema(schema, { target: 'draft-2020-12' }), title };
  writeFileSync(join(outDir, file), `${JSON.stringify(jsonSchema, null, 2)}\n`);
  console.log(`wrote schema/${file}`);
}
