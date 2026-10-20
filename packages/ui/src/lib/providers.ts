import type { SourceKind, Status, WindowKind } from '@cooldown/snapshot-schema'
import { windowLabel } from './snapshot-view'

export interface ProviderInfo {
  id: string
  name: string
  /** Two letters for the monogram tile. Providers' own logos are not used. */
  mark: string
  /**
   * Hue (oklch) of the monogram tile, so providers can be told apart at a glance. Kept clear of the
   * status hues (tide, shoal, sand). `null` is neutral.
   */
  hue: number | null
  /** One sentence on how Cooldown gets this provider's numbers. Shown on the detail screen. */
  howObtained: string
  /** How often the number can change, in plain words. */
  freshness: string
}

const PROVIDERS: Record<string, ProviderInfo> = {
  claude: {
    id: 'claude',
    name: 'Claude',
    mark: 'Cl',
    hue: 45,
    howObtained:
      "Read from Claude Code's own statusline hook (rate_limits.five_hour and rate_limits.seven_day), written to a small file on this PC. Cooldown never reads your Claude login, tokens or browser cookies. The figure is only fresh while Claude Code is running, and per-model windows are not exposed, so they show as not available.",
    freshness: 'Updates each time Claude Code redraws its statusline, so only while Claude Code is running.',
  },
  codex: {
    id: 'codex',
    name: 'Codex',
    mark: 'Cx',
    hue: 155,
    howObtained:
      'Asked from the local Codex app-server (account/rateLimits/read), falling back to the rate-limit entries in Codex session logs. Both are undocumented and experimental, so this adapter is versioned and can degrade after a Codex update. A single odd read is not trusted: two consistent reads are needed before an alert is moved or cancelled.',
    freshness: 'Polled every few minutes while the PC is awake.',
  },
  openrouter: {
    id: 'openrouter',
    name: 'OpenRouter',
    mark: 'OR',
    hue: 295,
    howObtained:
      'A request to GET /api/v1/key using the API key you saved on this device. The key is sent only to OpenRouter and is never synced to other devices.',
    freshness: 'Polled every few minutes.',
  },
  deepseek: {
    id: 'deepseek',
    name: 'DeepSeek',
    mark: 'DS',
    hue: 262,
    howObtained:
      'A request to GET /user/balance using the API key you saved on this device (Bearer token). The key is sent only to DeepSeek and is never synced to other devices.',
    freshness: 'Polled every few minutes.',
  },
  xai: {
    id: 'xai',
    name: 'xAI',
    mark: 'xA',
    hue: null,
    howObtained:
      "Read from the xAI Management API's prepaid balance, which needs a management key (a normal API key does not work). The team is found from the key; only a key that covers a whole organization needs the team ID as well. Without a key this adapter stays off and no xAI balance is shown.",
    freshness: 'Polled every few minutes once a management key is saved.',
  },
  antigravity: {
    id: 'antigravity',
    name: 'Antigravity',
    mark: 'AG',
    hue: 185,
    howObtained:
      "Read from Antigravity CLI's own status line (its quota object: the fraction left and the reset time of each bucket), written to a small file on this PC by the Cooldown hook. Cooldown never reads your Google login. The figure is only fresh while Antigravity CLI is running.",
    freshness: 'Updates each time Antigravity CLI redraws its status line, so only while it is running.',
  },
  copilot: {
    id: 'copilot',
    name: 'GitHub Copilot',
    mark: 'GH',
    hue: 125,
    howObtained:
      "A request to GitHub's billing API (GET /users/{you}/settings/billing/ai_credit/usage) with the token you saved on this device. It reports the AI credits spent this calendar month. GitHub does not report your plan's allowance there, so the percentage uses the monthly allowance you enter.",
    freshness: 'Polled every 15 minutes. GitHub updates the billing figures with some delay.',
  },
  kimi: {
    id: 'kimi',
    name: 'Kimi',
    mark: 'Ki',
    hue: 15,
    howObtained:
      'A request to GET /v1/users/me/balance on the Kimi (Moonshot) API with the key you saved on this device. The key is sent only to Moonshot and is never synced to other devices.',
    freshness: 'Polled every few minutes.',
  },
}

export function providerInfo(id: string): ProviderInfo {
  return (
    PROVIDERS[id] ?? {
      id,
      name: id.charAt(0).toUpperCase() + id.slice(1),
      mark: (id.charAt(0).toUpperCase() + id.charAt(1)).slice(0, 2),
      hue: null,
      howObtained: 'Read by a local adapter on this device.',
      freshness: 'Updated by the adapter on its own schedule.',
    }
  )
}

const BALANCE_TITLES: Record<string, string> = {
  openrouter: 'Credit balance',
  deepseek: 'Top-up balance',
  xai: 'Prepaid balance',
  kimi: 'Prepaid balance',
}

/** Windows whose generic label would mislead, by `provider:window`. */
const WINDOW_TITLES: Record<string, string> = {
  'copilot:monthly': 'AI credits this month',
}

/** "5-hour session", "Weekly", or a provider-specific title for balances. */
export function windowTitle(provider: string, window: WindowKind): string {
  const specific = WINDOW_TITLES[`${provider}:${window}`]
  if (specific) return specific
  if (window === 'balance') return BALANCE_TITLES[provider] ?? windowLabel(window)
  return windowLabel(window)
}

export const SOURCE_COPY: Record<SourceKind, { label: string; meaning: string }> = {
  official: {
    label: 'Official',
    meaning: 'Reported by the provider or by its own tool.',
  },
  estimated: {
    label: 'Estimated',
    meaning: 'Worked out from other data, so it can be off.',
  },
  scraped: {
    label: 'Scraped',
    meaning: 'Read from an undocumented source that can change without notice.',
  },
  manual: {
    label: 'Manual',
    meaning: 'Typed in by you. It only changes when you edit it.',
  },
}

export const STATUS_COPY: Record<Status, { label: string; meaning: string }> = {
  ok: { label: 'Current', meaning: 'The adapter read this recently and nothing is wrong.' },
  stale: { label: 'Stale', meaning: 'Not refreshed lately, or the window it describes has already ended. The last good value is kept.' },
  degraded: { label: 'Degraded', meaning: 'The adapter is struggling or falling back to a weaker source. The value may be off.' },
  unavailable: { label: 'Unavailable', meaning: 'The adapter cannot read this right now. The last good value is kept.' },
}

const ADAPTER_SUMMARY: Record<string, string> = {
  'claude-code-statusline': "From Claude Code's statusline hook.",
  'codex-app-server': 'Asked from the local Codex app-server.',
  'codex-session-logs': 'Estimated from Codex session logs.',
  'openrouter-key': 'Read with your OpenRouter key.',
  'deepseek-balance': 'Read with your DeepSeek key.',
  'xai-management': 'Read with your xAI management key.',
  'antigravity-statusline': "From Antigravity CLI's status line hook.",
  copilot: 'Read from the GitHub billing API with your token.',
  kimi: 'Read with your Kimi key.',
  'manual-entry': 'Typed in by hand.',
}

/** One short line on where a reading comes from, shown on cards that have nothing wrong to report. */
export function adapterSummary(adapter: string): string {
  return ADAPTER_SUMMARY[adapter] ?? `From the ${adapter} adapter.`
}
