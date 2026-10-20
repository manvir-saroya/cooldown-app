// Builds the Claude Code statusline hook and puts it where Tauri expects a sidecar:
// src-tauri/binaries/cooldown-statusline-<target triple>[.exe].
//
//   node scripts/build-sidecar.mjs            release build for the host
//   node scripts/build-sidecar.mjs --debug    debug build (faster, for tests)
//   TAURI_TARGET_TRIPLE=x86_64-pc-windows-msvc node scripts/build-sidecar.mjs
import { execFileSync } from 'node:child_process'
import { copyFileSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const repo = join(here, '..', '..', '..')
const debug = process.argv.includes('--debug')

function hostTriple() {
  const out = execFileSync('rustc', ['-vV'], { encoding: 'utf8' })
  const line = out.split('\n').find((l) => l.startsWith('host:'))
  if (!line) throw new Error('rustc -vV did not report a host triple')
  return line.slice('host:'.length).trim()
}

const triple = process.env.TAURI_TARGET_TRIPLE || process.env.TAURI_ENV_TARGET_TRIPLE || hostTriple()
const exe = triple.includes('windows') ? '.exe' : ''
const profile = debug ? 'debug' : 'release'

const args = ['build', '-p', 'cooldown-statusline', '--bin', 'cooldown-statusline', '--target', triple]
if (!debug) args.push('--release')
execFileSync('cargo', args, { cwd: repo, stdio: 'inherit' })

const built = join(repo, 'target', triple, profile, `cooldown-statusline${exe}`)
const dest = join(here, '..', 'src-tauri', 'binaries', `cooldown-statusline-${triple}${exe}`)
mkdirSync(dirname(dest), { recursive: true })
copyFileSync(built, dest)
console.log(`sidecar ready: ${dest}`)
