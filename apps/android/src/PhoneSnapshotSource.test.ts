import { describe, expect, it, vi } from 'vitest'
import type { Snapshot } from '@cooldown/snapshot-schema'
import type { SnapshotSource } from '@cooldown/ui'
import { CHANGED_EVENT, PhoneSnapshotSource, type InvokeFn, type ListenFn } from './PhoneSnapshotSource'

function setup(reply: (command: string, args?: Record<string, unknown>) => unknown = () => undefined) {
  const calls: { command: string; args?: Record<string, unknown> }[] = []
  const invokeFn = (async (command: string, args?: Record<string, unknown>) => {
    calls.push({ command, args })
    return reply(command, args)
  }) as InvokeFn
  const handlers = new Map<string, () => void>()
  const stops = new Map<string, ReturnType<typeof vi.fn>>()
  const listenFn: ListenFn = vi.fn(async (event, handler) => {
    handlers.set(event, handler)
    const stop = vi.fn()
    stops.set(event, stop)
    return stop
  })
  const source = new PhoneSnapshotSource(invokeFn, listenFn)
  return { source, calls, listenFn, handlers, stops, fire: (event: string) => handlers.get(event)?.() }
}

const claude: Snapshot = {
  schemaVersion: 1,
  id: 'claude:personal:session_5h',
  provider: 'claude',
  account: 'personal',
  window: 'session_5h',
  unit: 'percent',
  percentUsed: 54,
  resetsAt: '2026-10-16T02:00:00Z',
  observedAt: '2026-10-16T00:00:00Z',
  source: { kind: 'official', adapter: 'claude-statusline', adapterVersion: '1.0.0' },
  status: 'ok',
}

const balance: Snapshot = {
  schemaVersion: 1,
  id: 'openrouter:personal:balance',
  provider: 'openrouter',
  account: 'personal',
  window: 'balance',
  unit: 'usd',
  used: 14.62,
  limit: 50,
  remaining: 35.38,
  observedAt: '2026-10-16T00:00:00Z',
  source: { kind: 'official', adapter: 'openrouter', adapterVersion: '1.0.0' },
  status: 'ok',
}

describe('PhoneSnapshotSource', () => {
  it('is real data from the PC, with alerts and phone control', () => {
    const { source } = setup()
    expect(source.isSample).toBe(false)
    expect(source.label).toBe('Your PC')
    expect(source.alerts).toBeDefined()
    expect(source.phone).toBeDefined()
    // The PC-side pairing control (confirm a phone's code) does not exist on a phone.
    expect((source as SnapshotSource).pairing).toBeUndefined()
  })

  it('maps reads and phone control to their commands with camelCase arguments', async () => {
    const { source, calls } = setup()
    await source.getSnapshots()
    await source.alerts.list()
    await source.phone.scanQr()
    await source.phone.beginPairing('CD1:abc')
    await source.phone.finishPairing()
    await source.phone.pairedPcs()
    await source.phone.unpair('pc-1')
    await source.phone.reconnect()
    await source.phone.permissions()
    await source.phone.askPermissions()
    expect(calls).toEqual([
      { command: 'get_snapshots', args: undefined },
      { command: 'get_alerts', args: undefined },
      { command: 'scan_qr', args: undefined },
      { command: 'pair_begin', args: { qr: 'CD1:abc' } },
      { command: 'pair_finish', args: undefined },
      { command: 'paired_pcs', args: undefined },
      { command: 'unpair', args: { pcId: 'pc-1' } },
      { command: 'reconnect', args: undefined },
      { command: 'alarm_permissions', args: undefined },
      { command: 'request_permissions', args: undefined },
    ])
  })

  it('sends bell changes to the PC as set_alert, and removing a bell pauses it there', async () => {
    const { source, calls } = setup()
    await source.alerts.set({ id: 'reset:claude:personal:session_5h', fireAt: '2026-10-16T02:00:00Z', enabled: true })
    await source.alerts.set({ id: 'reset:claude:personal:weekly', fireAt: '2026-10-20T02:00:00Z', enabled: false })
    await source.alerts.remove('reset:claude:personal:session_5h')
    expect(calls).toEqual([
      { command: 'set_alert', args: { id: 'reset:claude:personal:session_5h', enabled: true } },
      { command: 'set_alert', args: { id: 'reset:claude:personal:weekly', enabled: false } },
      { command: 'set_alert', args: { id: 'reset:claude:personal:session_5h', enabled: false } },
    ])
  })

  it('does not refetch after a bell change: the PC answers with a sync, and an early refetch would undo the change on screen', async () => {
    const { source } = setup()
    const listener = vi.fn()
    source.subscribe(listener)
    await source.alerts.set({ id: 'reset:claude:personal:session_5h', fireAt: '2026-10-16T02:00:00Z', enabled: true })
    await source.alerts.remove('reset:claude:personal:session_5h')
    expect(listener).not.toHaveBeenCalled()
  })

  it('turns backend error strings into Errors, including a cancelled scan', async () => {
    const { source } = setup((command) => {
      if (command === 'scan_qr') throw 'cancelled'
      throw 'Your PC did not answer.'
    })
    await expect(source.getSnapshots()).rejects.toThrow('Your PC did not answer.')
    await expect(source.phone.scanQr()).rejects.toThrow('cancelled')
    await expect(source.phone.scanQr()).rejects.toBeInstanceOf(Error)
    await expect(source.alerts.set({ id: 'x', fireAt: '2026-10-16T02:00:00Z', enabled: true })).rejects.toBeInstanceOf(Error)
  })

  it('refuses keys, the hook and pairing offers with a kind sentence and never calls the backend', async () => {
    const { source, calls } = setup()
    await expect(source.saveApiKey('openrouter', 'sk-secret')).rejects.toThrow('Keys stay on your PC')
    await expect(source.removeApiKey('openrouter')).rejects.toThrow('Keys stay on your PC')
    await expect(source.installHook()).rejects.toThrow('lives on your PC')
    await expect(source.uninstallHook()).rejects.toThrow('lives on your PC')
    await expect(source.getHookStatus()).rejects.toThrow('lives on your PC')
    await expect(source.createPairingPayload()).rejects.toThrow('scans a code from your PC')
    await expect(source.revokeDevice('00ff')).rejects.toThrow('PC')
    expect(calls).toHaveLength(0)
  })

  it('has no adapters and no paired phones of its own', async () => {
    const { source, calls } = setup()
    expect(await source.getAdapters()).toEqual([])
    expect(await source.getPairedDevices()).toEqual([])
    expect(calls).toHaveLength(0)
  })

  it('answers history with just the current reading, since the phone keeps none', async () => {
    const { source, calls } = setup(() => [claude, balance])
    const usage = await source.getHistory(claude.id, '7d')
    expect(usage).toEqual({
      snapshotId: claude.id,
      unit: 'percent',
      kind: 'usage',
      range: '7d',
      points: [{ t: Date.parse(claude.observedAt), value: 54 }],
      resets: [],
    })
    const money = await source.getHistory(balance.id, '30d')
    expect(money.kind).toBe('balance')
    expect(money.points).toEqual([{ t: Date.parse(balance.observedAt), value: 35.38 }])
    await expect(source.getHistory('nope', '24h')).rejects.toThrow('not on this phone')
    expect(calls.every((c) => c.command === 'get_snapshots')).toBe(true)
  })

  it('normalises what the backend returns for PCs and permissions', async () => {
    const { source } = setup((command) =>
      command === 'paired_pcs'
        ? [{ id: 'pc-1', name: 'Desk PC', pairedAt: Date.parse('2026-10-14T10:00:00Z'), lastSeenAt: '2026-10-15T09:00:00Z', connected: 1 }]
        : { notifications: true, exactAlarms: undefined },
    )
    expect(await source.phone.pairedPcs()).toEqual([
      { id: 'pc-1', name: 'Desk PC', pairedAt: '2026-10-14T10:00:00.000Z', lastSeenAt: '2026-10-15T09:00:00Z', connected: false },
    ])
    expect(await source.phone.permissions()).toEqual({ notifications: true, exactAlarms: false, localNetwork: false })
    expect(await source.phone.askPermissions()).toEqual({ notifications: true, exactAlarms: false, localNetwork: false })
  })

  it('listens for the change event once, relays it, and stops when the last listener leaves', async () => {
    const { source, listenFn, stops, fire } = setup()
    const a = vi.fn()
    const b = vi.fn()
    const offA = source.subscribe(a)
    const offB = source.subscribe(b)
    expect(listenFn).toHaveBeenCalledTimes(1)
    expect(vi.mocked(listenFn).mock.calls[0]?.[0]).toBe(CHANGED_EVENT)
    await Promise.resolve()
    fire(CHANGED_EVENT)
    expect(a).toHaveBeenCalledTimes(1)
    expect(b).toHaveBeenCalledTimes(1)
    offA()
    expect(stops.get(CHANGED_EVENT)).not.toHaveBeenCalled()
    offB()
    await vi.waitFor(() => {
      expect(stops.get(CHANGED_EVENT)).toHaveBeenCalledTimes(1)
    })
  })

  it('tells subscribers after changes the backend does not always announce', async () => {
    const { source } = setup()
    const listener = vi.fn()
    source.subscribe(listener)
    await source.phone.finishPairing()
    await source.phone.unpair('pc-1')
    await source.phone.reconnect()
    await source.refresh()
    expect(listener).toHaveBeenCalledTimes(4)
  })

  it('does not tell subscribers when the change failed', async () => {
    const { source } = setup(() => {
      throw 'Your PC did not answer.'
    })
    const listener = vi.fn()
    source.subscribe(listener)
    await expect(source.refresh()).rejects.toThrow('Your PC did not answer.')
    await expect(source.phone.reconnect()).rejects.toThrow()
    expect(listener).not.toHaveBeenCalled()
  })

  it('maps the widget settings to their commands and tidies what comes back', async () => {
    const { source, calls } = setup((command) => {
      if (command === 'pin_widget') return { requested: true }
      return { windows: ['claude:personal:session_5h', 7, null], canPin: true }
    })
    const widget = source.phone.widget!
    expect(await widget.getSettings()).toEqual({ windows: ['claude:personal:session_5h'], canPin: true })
    await widget.setWindows(['claude:personal:session_5h'])
    await widget.setWindows(null)
    expect(await widget.pin()).toBe(true)
    expect(calls.map((c) => c.command)).toEqual(['widget_config', 'set_widget_config', 'set_widget_config', 'pin_widget'])
    expect(calls[1]?.args).toEqual({ windows: ['claude:personal:session_5h'] })
    expect(calls[2]?.args).toEqual({ windows: null })
  })

  it('reads a missing or empty widget choice as every window and a missing canPin as no', async () => {
    const { source } = setup((command) => (command === 'widget_config' ? { windows: [] } : null))
    expect(await source.phone.widget!.getSettings()).toEqual({ windows: null, canPin: false })
    const none = setup(() => null)
    expect(await none.source.phone.widget!.getSettings()).toEqual({ windows: null, canPin: false })
  })

  it('says the launcher cannot place the widget when the backend says it did not ask', async () => {
    const { source } = setup(() => ({ requested: false }))
    expect(await source.phone.widget!.pin()).toBe(false)
    const empty = setup(() => undefined)
    expect(await empty.source.phone.widget!.pin()).toBe(false)
  })

  it('reads and saves the phone notification settings, which have no "limit is back"', async () => {
    const { source, calls } = setup((command) => (command === 'get_notifications' ? { ranOut: true, lowAtPercent: 90 } : null))
    expect(await source.notifications.get()).toEqual({ ranOut: true, lowAtPercent: 90 })
    await source.notifications.set({ limitBack: true, ranOut: false, lowAtPercent: null })
    expect(calls.map((c) => [c.command, c.args])).toEqual([
      ['get_notifications', undefined],
      ['set_notifications', { notifications: { ranOut: false, lowAtPercent: null } }],
    ])
  })
  it('turns widget errors into Errors', async () => {
    const { source } = setup(() => {
      throw 'Pick at least one window, or show all of them.'
    })
    await expect(source.phone.widget!.setWindows([])).rejects.toThrow('Pick at least one window')
  })
})
