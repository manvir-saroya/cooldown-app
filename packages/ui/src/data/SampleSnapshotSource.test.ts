import { describe, expect, it, vi } from 'vitest'
import { SOURCE_COPY } from '@/lib/providers'
import { effectiveStatus, parseTime } from '@/lib/snapshot-view'
import { SampleSnapshotSource } from './SampleSnapshotSource'

const NOW = Date.parse('2026-10-10T12:00:00Z')
const make = () => new SampleSnapshotSource({ now: () => NOW })

describe('SampleSnapshotSource', () => {
  it('covers the required honest states and a reset a few minutes away', async () => {
    const snaps = await make().getSnapshots()
    const statuses = new Set(snaps.map((s) => s.status))
    expect(statuses).toEqual(new Set(['ok', 'stale', 'degraded', 'unavailable']))
    const soon = snaps.filter((s) => {
      const at = parseTime(s.resetsAt)
      return at !== undefined && at - NOW > 0 && at - NOW < 10 * 60_000
    })
    expect(soon.length).toBeGreaterThanOrEqual(1)
  })

  it('serves every provider in the brief and every source kind the app really produces', async () => {
    const snaps = await make().getSnapshots()
    expect(new Set(snaps.map((s) => s.provider))).toEqual(new Set(['claude', 'codex', 'openrouter', 'deepseek', 'xai']))
    // No adapter or Settings field enters a balance by hand, so the sample shows no "manual" reading
    // (the contract still allows the kind, and SOURCE_COPY still explains it).
    expect(new Set(snaps.map((s) => s.source.kind))).toEqual(new Set(['official', 'estimated', 'scraped']))
    expect(SOURCE_COPY.manual.label).toBe('Manual')
  })

  it('never tells the user to enter a reading by hand', async () => {
    const source = make()
    const text = JSON.stringify([await source.getSnapshots(), await source.getAdapters()])
    expect(text).not.toMatch(/by hand|typed in|manual-entry/i)
  })

  it('keeps the last good value on unavailable snapshots instead of blanking them', async () => {
    const snaps = await make().getSnapshots()
    const dead = snaps.find((s) => s.status === 'unavailable')
    expect(dead?.remaining).toBeTypeOf('number')
    expect(dead?.note).toBeTruthy()
  })

  it('builds ids that follow the provider:account:window contract', async () => {
    for (const s of await make().getSnapshots()) {
      expect(s.id).toBe(`${s.provider}:${s.account}:${s.window}`)
    }
  })

  it('treats a snapshot as stale once its reset passes', async () => {
    const snaps = await make().getSnapshots()
    const codex = snaps.find((s) => s.id === 'codex:personal:session_5h')!
    expect(effectiveStatus(codex, NOW).status).toBe('ok')
    expect(effectiveStatus(codex, NOW + 10 * 60_000).status).toBe('stale')
  })

  it('draws history that ends on the snapshot value, for usage and balances', async () => {
    const source = make()
    const usage = await source.getHistory('claude:personal:session_5h', '24h')
    expect(usage.kind).toBe('usage')
    expect(usage.points.at(-1)?.value).toBe(54)
    expect(usage.resets.length).toBeGreaterThan(0)
    const balance = await source.getHistory('openrouter:personal:balance', '30d')
    expect(balance.kind).toBe('balance')
    expect(balance.points.at(-1)?.value).toBeCloseTo(35.38, 2)
    // Deterministic: the same call gives the same curve.
    expect((await source.getHistory('claude:personal:session_5h', '24h')).points).toEqual(usage.points)
  })

  it('notifies subscribers when data changes, and stops after unsubscribe', async () => {
    const source = make()
    const listener = vi.fn()
    const off = source.subscribe(listener)
    await source.revokeDevice('dev-pixel-9')
    expect(listener).toHaveBeenCalledTimes(1)
    expect((await source.getPairedDevices()).map((d) => d.id)).toEqual(['dev-tab-s9'])
    off()
    await source.uninstallHook()
    expect(listener).toHaveBeenCalledTimes(1)
  })

  it('never hands a saved key back', async () => {
    const source = make()
    await source.saveApiKey('xai-management', 'sk-secret-value', { teamId: 't' })
    const adapters = await source.getAdapters()
    expect(JSON.stringify(adapters)).not.toContain('sk-secret-value')
    expect(adapters.find((a) => a.id === 'xai-management')?.hasKey).toBe(true)
    await expect(source.saveApiKey('xai-management', '   ')).rejects.toThrow()
  })

  it('gives a fresh pairing code each time, marked as a placeholder', async () => {
    const source = make()
    const a = await source.createPairingPayload()
    const b = await source.createPairingPayload()
    expect(a.manualCode).not.toBe(b.manualCode)
    // The current QR text shape (`CD1:` + base32), not the old project's `RB-` prefix.
    expect(a.manualCode).toMatch(/^CD1:[A-Z2-7]+$/)
    expect(a.uri).toMatch(/^CD1:[A-Z2-7]+$/)
    expect(a.placeholder).toBe(true)
    expect(Date.parse(a.expiresAt)).toBeGreaterThan(NOW)
  })

  it('with liveUpdates, publishes a fresh empty window shortly after a reset passes', async () => {
    vi.useFakeTimers()
    vi.setSystemTime(NOW)
    try {
      const source = new SampleSnapshotSource({ liveUpdates: true })
      const listener = vi.fn()
      source.subscribe(listener)
      const before = (await source.getSnapshots()).find((s) => s.id === 'codex:personal:session_5h')!
      expect(before.percentUsed).toBe(100)

      await vi.advanceTimersByTimeAsync(4 * 60_000 + 40_000 + 7_000)
      expect(listener).toHaveBeenCalled()
      const after = (await source.getSnapshots()).find((s) => s.id === 'codex:personal:session_5h')!
      expect(after.percentUsed).toBe(0)
      expect(Date.parse(after.resetsAt!)).toBeGreaterThan(Date.parse(before.resetsAt!) + 4 * 3_600_000)
      source.dispose()
    } finally {
      vi.useRealTimers()
    }
  })
})
