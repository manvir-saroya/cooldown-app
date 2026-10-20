#!/usr/bin/env node
// Fills .github/release-notes.template.md in. Everything comes from environment variables:
//   VERSION, WINDOWS_ASSET, ANDROID_ASSET   names of the two release files
//   SIGNED          "true" for an APK signed with the release key, anything else for debug-signed
//   CERT_SHA256     the APK's signing certificate fingerprint (used when SIGNED is "true")
//   CHECKSUMS_FILE  path of SHA256SUMS.txt; its contents are pasted into the notes
//   RUN_URL, COMMIT where the build ran
//   OUT             where to write the notes
//
// Template syntax: {{NAME}} is replaced; {{#signed}}...{{/signed}} is kept only for a release-key
// APK and {{^signed}}...{{/signed}} only for a debug-signed one.
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const env = (name) => {
  const value = process.env[name];
  if (value === undefined || value === '') throw new Error(`${name} is not set`);
  return value;
};

const signed = process.env.SIGNED === 'true';
const values = {
  VERSION: env('VERSION'),
  WINDOWS_ASSET: env('WINDOWS_ASSET'),
  ANDROID_ASSET: env('ANDROID_ASSET'),
  CERT_SHA256: signed ? env('CERT_SHA256') : '',
  CHECKSUMS: readFileSync(env('CHECKSUMS_FILE'), 'utf8').trim(),
  RUN_URL: env('RUN_URL'),
  COMMIT: env('COMMIT'),
};

const template = readFileSync(join(dirname(fileURLToPath(import.meta.url)), '..', 'release-notes.template.md'), 'utf8');

const keep = (text, tag, wanted) =>
  text.replace(new RegExp(`\\{\\{${tag}\\}\\}\\n([\\s\\S]*?)\\{\\{/signed\\}\\}\\n?`, 'g'), (_, body) => (wanted ? body : ''));

let notes = keep(keep(template, '#signed', signed), '\\^signed', !signed);
notes = notes.replace(/\{\{([A-Z0-9_]+)\}\}/g, (_, name) => {
  if (!(name in values)) throw new Error(`The template uses {{${name}}}, which has no value`);
  return values[name];
});
const leftover = notes.match(/\{\{[^}]*\}\}/);
if (leftover) throw new Error(`Unfilled placeholder in the notes: ${leftover[0]}`);

writeFileSync(env('OUT'), notes);
console.log(`Wrote ${process.env.OUT} (${notes.length} characters)`);
