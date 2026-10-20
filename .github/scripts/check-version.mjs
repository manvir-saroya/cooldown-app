#!/usr/bin/env node
// Fails unless every place that declares Cooldown's version agrees. On a tag build it also
// requires the tag to be "v" plus that version. Prints the version and, under GitHub Actions,
// writes it to $GITHUB_OUTPUT as `version`.
//
// Run it by hand from anywhere: node .github/scripts/check-version.mjs
import { appendFileSync, existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const read = (path) => readFileSync(join(root, path), 'utf8');
const found = []; // [where, version]

// Cargo workspace: the version every crate inherits.
const workspaceSection = read('Cargo.toml')
  .split(/^\[/m)
  .find((section) => section.startsWith('workspace.package]'));
const cargoVersion = workspaceSection?.match(/^version\s*=\s*"([^"]+)"/m)?.[1];
if (!cargoVersion) throw new Error('Cargo.toml: no [workspace.package] version');
found.push(['Cargo.toml [workspace.package]', cargoVersion]);

// Cargo.lock: the workspace crates must have been re-locked after a bump.
const lockedCrates = [...read('Cargo.lock').matchAll(/name = "(cooldown-[^"]+)"\r?\nversion = "([^"]+)"/g)];
if (lockedCrates.length === 0) throw new Error('Cargo.lock: no cooldown-* crates found');
for (const [, name, version] of lockedCrates) found.push([`Cargo.lock ${name}`, version]);

// Every npm workspace package, and the root.
const packageDirs = ['.'];
for (const group of ['packages', 'apps']) {
  for (const entry of readdirSync(join(root, group), { withFileTypes: true })) {
    if (entry.isDirectory() && existsSync(join(root, group, entry.name, 'package.json'))) {
      packageDirs.push(`${group}/${entry.name}`);
    }
  }
}
for (const dir of packageDirs) {
  const file = join(dir, 'package.json');
  found.push([file, JSON.parse(read(file)).version]);
}

// Tauri configs (the installer and the APK take their version from these).
for (const dir of ['apps/desktop/src-tauri', 'apps/android/src-tauri']) {
  const file = join(dir, 'tauri.conf.json');
  found.push([file, JSON.parse(read(file)).version]);
}

const version = cargoVersion;
const wrong = found.filter(([, v]) => v !== version);
for (const [where, v] of found) console.log(`${v === version ? 'ok      ' : 'MISMATCH'} ${v}  ${where}`);
if (wrong.length > 0) {
  console.error(`::error::${wrong.length} version declaration(s) differ from ${version} (Cargo.toml)`);
  process.exit(1);
}
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  console.error(`::error::"${version}" is not a plain semantic version`);
  process.exit(1);
}

if (process.env.GITHUB_REF_TYPE === 'tag' && process.env.GITHUB_REF_NAME !== `v${version}`) {
  console.error(
    `::error::Tag ${process.env.GITHUB_REF_NAME} does not match the version in the repo (v${version}). ` +
      'Delete the tag, fix the version and tag again.',
  );
  process.exit(1);
}

console.log(`Version ${version}`);
if (process.env.GITHUB_OUTPUT) appendFileSync(process.env.GITHUB_OUTPUT, `version=${version}\n`);
