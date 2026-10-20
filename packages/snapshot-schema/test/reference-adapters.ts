// TEST-ONLY executable specification of the six adapters' mapping rules.
//
// It exists so the hand-checked `*.expected.json` files cannot drift from the rules written in
// fixtures/README.md: the fixtures test runs every case through these functions and requires an
// exact match. The production adapters are written in Rust (apps/desktop) and are checked against
// the same fixture files; nothing in the shipped packages imports this file.

import { ADAPTERS, type AdapterId, type FixtureContext } from '../src/adapters';
import { windowFromDurationMinutes } from '../src/helpers';
import type { Snapshot, WindowKind } from '../src/types';

type Obj = Record<string, unknown>;

export const NOTE_RESET = 'window reset';
export const NOTE_PAST_RESET = 'Reset time has already passed; waiting for a fresh reading';

const isObj = (value: unknown): value is Obj =>
  typeof value === 'object' && value !== null && !Array.isArray(value);
const num = (value: unknown): number | undefined =>
  typeof value === 'number' && Number.isFinite(value) ? value : undefined;
const clampPercent = (value: number): number => Math.min(100, Math.max(0, value));
const round = (value: number, decimals: number): number => {
  const factor = 10 ** decimals;
  return Math.round(value * factor) / factor + 0; // "+ 0" turns -0 into 0
};
const isoFromSeconds = (seconds: number): string =>
  new Date(Math.floor(seconds) * 1000).toISOString().replace(/\.\d{3}Z$/, 'Z');
const truncate = (text: string, max: number): string => (text.length > max ? text.slice(0, max) : text);

/** Everything a snapshot needs except the per-window fields. */
function make(
  adapterId: AdapterId,
  ctx: FixtureContext,
  window: WindowKind,
  fields: Partial<Snapshot>,
): Snapshot {
  const adapter = ADAPTERS.find((entry) => entry.id === adapterId)!;
  const snapshot: Snapshot = {
    schemaVersion: 1,
    id: `${adapter.provider}:${ctx.account}:${window}`,
    provider: adapter.provider,
    account: ctx.account,
    window,
    unit: window === 'balance' ? 'usd' : 'percent',
    observedAt: ctx.now,
    source: { kind: adapter.sourceKind, adapter: adapter.id, adapterVersion: ctx.adapterVersion },
    status: 'ok',
    ...fields,
  };
  for (const key of Object.keys(snapshot) as Array<keyof Snapshot>) {
    if (snapshot[key] === undefined) delete snapshot[key];
  }
  return snapshot;
}

const hasValues = (s: Snapshot): boolean =>
  s.percentUsed !== undefined || s.used !== undefined || s.remaining !== undefined;

/**
 * No fresh value for this window. Keep the last good value if there is one ('degraded', or
 * 'stale' when its reset time has passed); otherwise emit 'unavailable'.
 */
function carryOrUnavailable(
  adapterId: AdapterId,
  ctx: FixtureContext,
  window: WindowKind,
  reason: string,
  unavailableNote: string = reason,
): Snapshot {
  const adapter = ADAPTERS.find((entry) => entry.id === adapterId)!;
  const id = `${adapter.provider}:${ctx.account}:${window}`;
  const previous = ctx.previous?.find((snapshot) => snapshot.id === id);
  if (previous && hasValues(previous)) {
    const expired = previous.resetsAt !== undefined && Date.parse(previous.resetsAt) <= Date.parse(ctx.now);
    return {
      ...previous,
      status: expired ? 'stale' : 'degraded',
      note: expired
        ? `${reason}; the last known value is past its reset time`
        : `${reason}; showing the last known value`,
    };
  }
  return make(adapterId, ctx, window, { status: 'unavailable', note: unavailableNote });
}

/** A usable percentage window: 'ok', or 'stale' when its reset time is already in the past. */
function percentWindow(
  adapterId: AdapterId,
  ctx: FixtureContext,
  window: WindowKind,
  percent: number,
  resetsAtSeconds: number | undefined,
  observedAt: string,
): Snapshot {
  const resetsAt = resetsAtSeconds !== undefined && resetsAtSeconds > 0 ? isoFromSeconds(resetsAtSeconds) : undefined;
  const stale = resetsAt !== undefined && Date.parse(resetsAt) <= Date.parse(ctx.now);
  return make(adapterId, ctx, window, {
    percentUsed: clampPercent(percent),
    resetsAt,
    observedAt,
    status: stale ? 'stale' : 'ok',
    note: stale ? NOTE_PAST_RESET : undefined,
  });
}

// ---------------------------------------------------------------------------------------------
// claude-statusline

const CLAUDE_NONE =
  'Claude Code has not reported this window (it only does for Pro/Max subscribers, after the first response of a session)';
const CLAUDE_MISSING = 'Window missing from the latest Claude Code update';

export function claudeStatusline(input: unknown, ctx: FixtureContext): Snapshot[] {
  const rateLimits = isObj(input) && isObj(input.rate_limits) ? input.rate_limits : undefined;
  const observedAt = ctx.observedAt ?? ctx.now;
  const slots: Array<[WindowKind, string]> = [
    ['session_5h', 'five_hour'],
    ['weekly', 'seven_day'],
  ];
  return slots.map(([window, key]) => {
    const raw = rateLimits?.[key];
    const percent = isObj(raw) ? num(raw.used_percentage) : undefined;
    if (isObj(raw) && percent !== undefined) {
      return percentWindow('claude-statusline', ctx, window, percent, num(raw.resets_at), observedAt);
    }
    // Claude Code drops a window once its resets_at passes: absent after a known reset = reset.
    const id = `claude:${ctx.account}:${window}`;
    const previous = ctx.previous?.find((snapshot) => snapshot.id === id);
    if (previous) {
      const ended =
        previous.resetsAt !== undefined
          ? Date.parse(previous.resetsAt) <= Date.parse(ctx.now)
          : previous.status === 'ok' && previous.note === NOTE_RESET;
      if (ended) return make('claude-statusline', ctx, window, { percentUsed: 0, note: NOTE_RESET });
    }
    return carryOrUnavailable('claude-statusline', ctx, window, CLAUDE_MISSING, CLAUDE_NONE);
  });
}

// ---------------------------------------------------------------------------------------------
// codex-app-server and codex-rollout-log share the window-mapping rules.

type CodexWindowKind = 'session_5h' | 'weekly';

interface CodexWindow {
  percent: number;
  resetsAtSeconds: number | undefined;
}

/** Assign windows by DURATION, not slot. Unknown durations are ignored. First window of a kind wins. */
function codexWindowsByDuration(
  slots: unknown[],
  keys: { used: string; minutes: string; resets: string },
): Map<CodexWindowKind, CodexWindow> {
  const mapped = new Map<CodexWindowKind, CodexWindow>();
  for (const slot of slots) {
    if (!isObj(slot)) continue;
    const percent = num(slot[keys.used]);
    const minutes = num(slot[keys.minutes]);
    if (percent === undefined || minutes === undefined) continue;
    const kind = windowFromDurationMinutes(minutes);
    if (kind !== 'session_5h' && kind !== 'weekly') continue;
    if (mapped.has(kind)) continue;
    const rawReset = num(slot[keys.resets]);
    // Seconds per the Codex protocol; tolerate milliseconds like the burnrate adapter does.
    const resetsAtSeconds =
      rawReset === undefined ? undefined : Math.abs(rawReset) < 10_000_000_000 ? rawReset : rawReset / 1000;
    mapped.set(kind, { percent, resetsAtSeconds });
  }
  return mapped;
}

function codexSnapshots(
  adapterId: AdapterId,
  ctx: FixtureContext,
  mapped: Map<CodexWindowKind, CodexWindow>,
  observedAt: string,
  noneNote: string,
): Snapshot[] {
  const windows: CodexWindowKind[] = ['session_5h', 'weekly'];
  return windows.map((window) => {
    const found = mapped.get(window);
    return found
      ? percentWindow(adapterId, ctx, window, found.percent, found.resetsAtSeconds, observedAt)
      : carryOrUnavailable(adapterId, ctx, window, noneNote);
  });
}

const CODEX_NONE = 'Codex did not report this window';

export function codexAppServer(input: unknown, ctx: FixtureContext): Snapshot[] {
  const failAll = (reason: string): Snapshot[] =>
    (['session_5h', 'weekly'] as const).map((window) => carryOrUnavailable('codex-app-server', ctx, window, reason));

  if (!isObj(input)) return failAll('Codex app-server returned no usable rate limits');
  if (isObj(input.error)) {
    const message = typeof input.error.message === 'string' ? truncate(input.error.message, 200) : 'unknown error';
    return failAll(`Codex app-server error: ${message}`);
  }
  const result = input.result;
  if (!isObj(result)) return failAll('Codex app-server returned no usable rate limits');
  const byLimitId = isObj(result.rateLimitsByLimitId) ? result.rateLimitsByLimitId : undefined;
  const bucket = isObj(byLimitId?.codex) ? byLimitId.codex : isObj(result.rateLimits) ? result.rateLimits : undefined;
  if (!bucket) return failAll('Codex app-server returned no usable rate limits');
  const mapped = codexWindowsByDuration([bucket.primary, bucket.secondary], {
    used: 'usedPercent',
    minutes: 'windowDurationMins',
    resets: 'resetsAt',
  });
  return codexSnapshots('codex-app-server', ctx, mapped, ctx.now, CODEX_NONE);
}

const ROLLOUT_NONE = 'No Codex rate-limit data in the latest rollout event';

export function codexRolloutLog(input: unknown, ctx: FixtureContext): Snapshot[] {
  const failAll = (): Snapshot[] =>
    (['session_5h', 'weekly'] as const).map((window) =>
      carryOrUnavailable('codex-rollout-log', ctx, window, ROLLOUT_NONE),
    );
  if (!isObj(input) || !isObj(input.payload) || input.payload.type !== 'token_count') return failAll();
  const rateLimits = input.payload.rate_limits;
  const eventMs = typeof input.timestamp === 'string' ? Date.parse(input.timestamp) : Number.NaN;
  if (!isObj(rateLimits) || Number.isNaN(eventMs)) return failAll();
  if (rateLimits.limit_id !== undefined && rateLimits.limit_id !== null && rateLimits.limit_id !== 'codex') {
    return failAll();
  }
  const mapped = codexWindowsByDuration([rateLimits.primary, rateLimits.secondary], {
    used: 'used_percent',
    minutes: 'window_minutes',
    resets: 'resets_at',
  });
  const observedAt = isoFromSeconds(eventMs / 1000);
  return codexSnapshots('codex-rollout-log', ctx, mapped, observedAt, ROLLOUT_NONE);
}

// ---------------------------------------------------------------------------------------------
// API-key adapters (single 'balance' window)

function httpFailure(name: string, ctx: FixtureContext): string | undefined {
  return ctx.httpStatus !== undefined && (ctx.httpStatus < 200 || ctx.httpStatus >= 300)
    ? `${name} request failed (HTTP ${ctx.httpStatus})`
    : undefined;
}

function balanceFailure(adapterId: AdapterId, ctx: FixtureContext, name: string): Snapshot[] {
  const reason = httpFailure(name, ctx) ?? `${name} response did not match the expected shape`;
  return [carryOrUnavailable(adapterId, ctx, 'balance', reason)];
}

export function openrouter(input: unknown, ctx: FixtureContext): Snapshot[] {
  const fail = () => balanceFailure('openrouter', ctx, 'OpenRouter');
  if (httpFailure('OpenRouter', ctx) !== undefined) return fail();
  const data = isObj(input) && isObj(input.data) ? input.data : undefined;
  const usage = data ? num(data.usage) : undefined;
  if (!data || usage === undefined || usage < 0) return fail();
  if (data.limit === null || data.limit === undefined) {
    return [make('openrouter', ctx, 'balance', { used: usage, note: 'No spending limit set on this key' })];
  }
  const limit = num(data.limit);
  const remaining = num(data.limit_remaining);
  if (limit === undefined || limit < 0 || remaining === undefined) return fail();
  const used = round(limit - remaining, 6);
  return [
    make('openrouter', ctx, 'balance', {
      percentUsed: limit > 0 ? round(clampPercent((used / limit) * 100), 4) : undefined,
      used,
      limit,
      remaining,
    }),
  ];
}

/** Decimal string to number; undefined unless it is plainly numeric and finite. */
function decimal(value: unknown): number | undefined {
  if (typeof value !== 'string' || value.trim() === '') return undefined;
  const parsed = Number(value.trim());
  return Number.isFinite(parsed) ? parsed : undefined;
}

export function deepseek(input: unknown, ctx: FixtureContext): Snapshot[] {
  const fail = () => balanceFailure('deepseek', ctx, 'DeepSeek');
  if (httpFailure('DeepSeek', ctx) !== undefined) return fail();
  if (!isObj(input) || typeof input.is_available !== 'boolean' || !Array.isArray(input.balance_infos)) return fail();
  const infos = input.balance_infos.filter(isObj);
  const usd = infos.filter((info) => info.currency === 'USD');
  const [entries, unit] = usd.length > 0 ? [usd, 'usd' as const] : [infos.filter((info) => info.currency === 'CNY'), 'cny' as const];
  if (entries.length !== 1) return fail();
  const total = decimal(entries[0]!.total_balance);
  if (total === undefined) return fail();
  return [
    make('deepseek', ctx, 'balance', {
      unit,
      remaining: round(total, 6),
      note: input.is_available ? undefined : 'DeepSeek reports the balance as too low for API calls',
    }),
  ];
}

export function xai(input: unknown, ctx: FixtureContext): Snapshot[] {
  const fail = () => balanceFailure('xai', ctx, 'xAI');
  if (httpFailure('xAI', ctx) !== undefined) return fail();
  const total = isObj(input) && isObj(input.total) ? decimal(input.total.val) : undefined;
  if (total === undefined) return fail();
  // Inverted ledger: purchases are negative, so the remaining balance is -cents / 100.
  const remaining = round(-total / 100, 6);
  return [
    make('xai', ctx, 'balance', {
      remaining,
      note: remaining < 0 ? 'Prepaid balance is negative (more spent than purchased)' : undefined,
    }),
  ];
}

export function kimi(input: unknown, ctx: FixtureContext): Snapshot[] {
  const fail = () => balanceFailure('kimi', ctx, 'Kimi');
  if (httpFailure('Kimi', ctx) !== undefined) return fail();
  if (!isObj(input) || input.code !== 0 || input.status !== true || !isObj(input.data)) return fail();
  const available = input.data.available_balance;
  if (typeof available !== 'number' || !Number.isFinite(available)) return fail();
  return [
    make('kimi', ctx, 'balance', {
      remaining: round(available, 6),
      note: available <= 0 ? 'Kimi reports no balance left for API calls' : undefined,
    }),
  ];
}

/** First instant of the month after `now`, as ISO-8601 UTC. */
function nextMonthStart(now: string): string {
  const d = new Date(now);
  return new Date(Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1)).toISOString().replace('.000Z', 'Z');
}

export function copilot(input: unknown, ctx: FixtureContext): Snapshot[] {
  const failed = httpFailure('GitHub', ctx);
  if (failed !== undefined) {
    const reason =
      ctx.httpStatus === 403 || ctx.httpStatus === 404
        ? `${failed}. The token needs the Plan permission (read) on your personal account`
        : failed;
    return [{ ...carryOrUnavailable('copilot', ctx, 'monthly', reason), unit: 'usd' }];
  }
  const items = isObj(input) && Array.isArray(input.usageItems) ? input.usageItems : undefined;
  const amounts = items?.map((item) => (isObj(item) ? item.grossAmount : undefined));
  if (!amounts || amounts.some((a) => typeof a !== 'number' || !Number.isFinite(a) || a < 0)) {
    return [{ ...carryOrUnavailable('copilot', ctx, 'monthly', 'GitHub response did not match the expected shape'), unit: 'usd' }];
  }
  const used = round((amounts as number[]).reduce((sum, a) => sum + a, 0), 6);
  return [make('copilot', ctx, 'monthly', { unit: 'usd', used, resetsAt: nextMonthStart(ctx.now) })];
}

const ANTIGRAVITY_NONE = 'Antigravity CLI has not reported a weekly quota (it sends one once a session has started)';
const ANTIGRAVITY_MISSING = 'Weekly quota missing from the latest Antigravity CLI update';

export function antigravityStatusline(input: unknown, ctx: FixtureContext): Snapshot[] {
  const observedAt = ctx.observedAt ?? ctx.now;
  const quota = isObj(input) && isObj(input.quota) ? input.quota : {};
  let tightest: { fraction: number; bucket: Record<string, unknown> } | undefined;
  for (const [id, bucket] of Object.entries(quota)) {
    if (!id.toLowerCase().includes('week') || !isObj(bucket)) continue;
    const fraction = num(bucket.remaining_fraction);
    if (fraction === undefined) continue;
    if (!tightest || fraction < tightest.fraction) tightest = { fraction, bucket };
  }
  if (!tightest) return [carryOrUnavailable('antigravity-statusline', ctx, 'weekly', ANTIGRAVITY_MISSING, ANTIGRAVITY_NONE)];
  const used = Math.round((1 - Math.min(1, Math.max(0, tightest.fraction))) * 100 * 10_000) / 10_000;
  const reset = typeof tightest.bucket.reset_time === 'string' ? Date.parse(tightest.bucket.reset_time) : NaN;
  return [percentWindow('antigravity-statusline', ctx, 'weekly', used, Number.isNaN(reset) ? undefined : Math.floor(reset / 1000), observedAt)];
}

export const REFERENCE_ADAPTERS: Record<AdapterId, (input: unknown, ctx: FixtureContext) => Snapshot[]> = {
  'claude-statusline': claudeStatusline,
  'codex-app-server': codexAppServer,
  'codex-rollout-log': codexRolloutLog,
  openrouter,
  deepseek,
  xai,
  kimi,
  copilot,
  'antigravity-statusline': antigravityStatusline,
};
