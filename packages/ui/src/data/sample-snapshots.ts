/**
 * Synthetic sample data. Every value here is invented for the UI and must stay
 * labelled "Sample data" wherever it is shown. Times are relative to `now` so
 * countdowns are live no matter when the app is opened.
 */
import type { Snapshot } from '@cooldown/snapshot-schema'
import type { AdapterInfo, HookStatus, PairedDevice } from './types'

const MIN = 60_000
const HOUR = 60 * MIN
const DAY = 24 * HOUR

const iso = (ms: number) => new Date(ms).toISOString()

export function buildSampleSnapshots(now: number): Snapshot[] {
  return [
    {
      schemaVersion: 1,
      id: 'claude:personal:session_5h',
      provider: 'claude',
      account: 'personal',
      window: 'session_5h',
      unit: 'percent',
      percentUsed: 54,
      resetsAt: iso(now + 2 * HOUR + 17 * MIN + 8_000),
      observedAt: iso(now - 2 * MIN),
      source: { kind: 'official', adapter: 'claude-code-statusline', adapterVersion: '0.1.0' },
      status: 'ok',
    },
    {
      schemaVersion: 1,
      id: 'claude:personal:weekly',
      provider: 'claude',
      account: 'personal',
      window: 'weekly',
      unit: 'percent',
      percentUsed: 38,
      resetsAt: iso(now + 3 * DAY + 5 * HOUR + 20 * MIN),
      observedAt: iso(now - 3 * HOUR - 12 * MIN),
      source: { kind: 'official', adapter: 'claude-code-statusline', adapterVersion: '0.1.0' },
      status: 'stale',
      note: 'Claude Code is not running, so the statusline hook has not written a new reading. This is the last one it saw.',
    },
    {
      schemaVersion: 1,
      id: 'codex:personal:session_5h',
      provider: 'codex',
      account: 'personal',
      window: 'session_5h',
      unit: 'percent',
      percentUsed: 100,
      // A few minutes away on purpose, so the countdown is visibly live.
      resetsAt: iso(now + 4 * MIN + 40_000),
      observedAt: iso(now - 40_000),
      source: { kind: 'scraped', adapter: 'codex-app-server', adapterVersion: '0.1.0' },
      status: 'ok',
    },
    {
      schemaVersion: 1,
      id: 'codex:personal:weekly',
      provider: 'codex',
      account: 'personal',
      window: 'weekly',
      unit: 'percent',
      percentUsed: 41,
      resetsAt: iso(now + 4 * DAY + 9 * HOUR),
      observedAt: iso(now - 27 * MIN),
      source: { kind: 'estimated', adapter: 'codex-session-logs', adapterVersion: '0.1.0' },
      status: 'degraded',
      note: 'The Codex app-server did not answer the last two reads. This figure is estimated from session logs and may be off.',
    },
    {
      schemaVersion: 1,
      id: 'openrouter:personal:balance',
      provider: 'openrouter',
      account: 'personal',
      window: 'balance',
      unit: 'usd',
      used: 14.62,
      limit: 50,
      remaining: 35.38,
      observedAt: iso(now - 1 * MIN),
      source: { kind: 'official', adapter: 'openrouter-key', adapterVersion: '0.1.0' },
      status: 'ok',
    },
    {
      schemaVersion: 1,
      id: 'deepseek:personal:balance',
      provider: 'deepseek',
      account: 'personal',
      window: 'balance',
      unit: 'usd',
      remaining: 8.9,
      observedAt: iso(now - 19 * HOUR),
      source: { kind: 'official', adapter: 'deepseek-balance', adapterVersion: '0.1.0' },
      status: 'unavailable',
      note: 'DeepSeek rejected the saved key (401). This is the last good balance. Update the key in Settings.',
    },
    {
      schemaVersion: 1,
      id: 'xai:team:balance',
      provider: 'xai',
      account: 'team',
      window: 'balance',
      unit: 'usd',
      used: 87.6,
      limit: 200,
      remaining: 112.4,
      observedAt: iso(now - 2 * DAY),
      source: { kind: 'official', adapter: 'xai-management', adapterVersion: '0.1.0' },
      status: 'stale',
      note: 'xAI has not answered for two days. This is the last good balance.',
    },
  ]
}

export function buildSampleAdapters(now: number): AdapterInfo[] {
  return [
    {
      id: 'claude-code-statusline',
      name: 'Claude Code statusline',
      provider: 'claude',
      kind: 'official',
      version: '0.1.0',
      status: 'stale',
      note: 'Waiting for Claude Code to run.',
      lastPolledAt: iso(now - 3 * HOUR - 12 * MIN),
      credentials: 'none',
      hasKey: false,
      enabled: true,
    },
    {
      id: 'codex-app-server',
      name: 'Codex app-server',
      provider: 'codex',
      kind: 'scraped',
      version: '0.1.0',
      status: 'degraded',
      note: 'No answer on the last two reads. Falling back to session logs.',
      lastPolledAt: iso(now - 27 * MIN),
      credentials: 'none',
      hasKey: false,
      enabled: true,
    },
    {
      id: 'openrouter-key',
      name: 'OpenRouter',
      provider: 'openrouter',
      kind: 'official',
      version: '0.1.0',
      status: 'ok',
      lastPolledAt: iso(now - 1 * MIN),
      credentials: 'api-key',
      hasKey: true,
      enabled: true,
    },
    {
      id: 'deepseek-balance',
      name: 'DeepSeek',
      provider: 'deepseek',
      kind: 'official',
      version: '0.1.0',
      status: 'unavailable',
      note: 'The saved key was rejected (401).',
      lastPolledAt: iso(now - 19 * HOUR),
      credentials: 'api-key',
      hasKey: true,
      enabled: true,
    },
    {
      id: 'xai-management',
      name: 'xAI',
      provider: 'xai',
      kind: 'official',
      version: '0.1.0',
      status: 'stale',
      note: 'xAI has not answered for two days. Showing the last good balance.',
      lastPolledAt: iso(now - 2 * DAY),
      credentials: 'management-key',
      hasKey: true,
      enabled: true,
    },
    {
      id: 'antigravity-statusline',
      name: 'Antigravity CLI statusline',
      provider: 'antigravity',
      kind: 'official',
      version: '0.1.0',
      status: 'unavailable',
      note: 'Switched off. It is not read and not shown.',
      credentials: 'none',
      hasKey: false,
      enabled: false,
      setup: 'Antigravity CLI (formerly Gemini CLI) passes its quota to its status line. Install the hook to connect it.',
      hookAgent: 'antigravity',
    },
    {
      id: 'copilot',
      name: 'GitHub Copilot',
      provider: 'copilot',
      kind: 'official',
      version: '0.1.0',
      status: 'unavailable',
      note: 'Switched off. It is not read and not shown.',
      credentials: 'api-key',
      hasKey: false,
      enabled: false,
      setup: 'AI credits spent this month, from the GitHub billing API. Needs a token that can read your plan.',
    },
    {
      id: 'kimi',
      name: 'Kimi (Moonshot)',
      provider: 'kimi',
      kind: 'official',
      version: '0.1.0',
      status: 'unavailable',
      note: 'Switched off. It is not read and not shown.',
      credentials: 'api-key',
      hasKey: false,
      enabled: false,
      setup: 'Prepaid balance of a platform.kimi.ai API key.',
    },
  ]
}

export function buildSampleDevices(now: number): PairedDevice[] {
  return [
    {
      id: 'dev-pixel-9',
      name: 'Pixel 9',
      platform: 'android',
      pairedAt: iso(now - 12 * DAY),
      lastSeenAt: iso(now - 4 * MIN),
    },
    {
      id: 'dev-tab-s9',
      name: 'Galaxy Tab S9',
      platform: 'android',
      pairedAt: iso(now - 40 * DAY),
      lastSeenAt: iso(now - 6 * DAY),
    },
  ]
}

export function buildSampleHook(now: number): HookStatus {
  return {
    installed: true,
    chainsExisting: true,
    settingsPath: '~/.claude/settings.json',
    backupPath: '~/.claude/settings.cooldown-backup.json',
    inboxPath: '~/.cooldown/inbox/claude-statusline.json',
    lastDataAt: iso(now - 3 * HOUR - 12 * MIN),
  }
}
