import { readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import type { FixtureContext } from '../src/adapters';
import type { Snapshot } from '../src/types';

export const PACKAGE_ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
export const FIXTURES_DIR = join(PACKAGE_ROOT, 'fixtures', 'adapters');

export interface FixtureCase {
  adapter: string;
  name: string;
  dir: string;
  input: unknown;
  context: FixtureContext;
  expected: Snapshot[];
  files: { input: string; context: string; expected: string };
}

export function readJson(path: string): unknown {
  return JSON.parse(readFileSync(path, 'utf8'));
}

export function adapterDirs(): string[] {
  return readdirSync(FIXTURES_DIR, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
}

export function caseNames(adapter: string): string[] {
  const names = new Set<string>();
  for (const file of readdirSync(join(FIXTURES_DIR, adapter))) {
    const match = /^(.+)\.(input|context|expected)\.json$/.exec(file);
    if (match) names.add(match[1]!);
  }
  return [...names].sort();
}

export function loadCase(adapter: string, name: string): FixtureCase {
  const dir = join(FIXTURES_DIR, adapter);
  const files = {
    input: join(dir, `${name}.input.json`),
    context: join(dir, `${name}.context.json`),
    expected: join(dir, `${name}.expected.json`),
  };
  return {
    adapter,
    name,
    dir,
    files,
    input: readJson(files.input),
    context: readJson(files.context) as FixtureContext,
    expected: readJson(files.expected) as Snapshot[],
  };
}

export function allCases(): FixtureCase[] {
  return adapterDirs().flatMap((adapter) => caseNames(adapter).map((name) => loadCase(adapter, name)));
}

export function allFixtureFiles(): string[] {
  return adapterDirs().flatMap((adapter) =>
    readdirSync(join(FIXTURES_DIR, adapter)).map((file) => join(FIXTURES_DIR, adapter, file)),
  );
}
