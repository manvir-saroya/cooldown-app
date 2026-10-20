import { describe, expect, it, vi } from 'vitest'
import { CHANGED_EVENT, TauriSnapshotSource, WINDOW_HIDDEN_EVENT, type InvokeFn, type ListenFn } from './TauriSnapshotSource'

function setup(reply: (command: string, args?: Record<string, unknown>) => unknown = () => undefined) {
  const calls: { command: string; args?: Record<string, unknown> }[] = []
  const invokeFn = (async (command: string, args?: Record<string, unknown>) => {
    calls.push({ command, args })
    return reply(command, args)
  }) as InvokeFn
  let handler: (() => void) | undefined
  const stop = vi.fn()
  const listenFn: ListenFn = vi.fn(async (event, h) => {
    expect(event).toBe(CHANGED_EVENT)
    handler = h
    return stop
  })
  const source = new TauriSnapshotSource(invokeFn, listenFn)
  return { source, calls, listenFn, stop, fire: () => handler?.() }
}

describe('TauriSnapshotSource', () => {
  it('is real data, not sample data', () => {
    const { source } = setup()
    expect(source.isSample).toBe(false)
    expect(source.alerts).toBeDefined()
    expect(source.pairing).toBeDefined()
  })

  it('maps each method to its command with camelCase arguments', async () => {
    const { source, calls } = setup()
    await source.getSnapshots()
    await source.getHistory('claude:default:weekly', '7d')
    await source.saveApiKey('xai', 'key-1', { teamId: 'team' })
    await source.saveApiKey('openrouter', 'key-2')
    await source.removeApiKey('openrouter')
    await source.revokeDevice('00ff')
    await source.alerts?.set({ id: 'reset:claude:default:weekly', fireAt: '2026-10-16T00:00:00Z', enabled: true })
    await source.alerts?.remove('reset:claude:default:weekly')
    await source.pairing?.confirm(true)
    expect(calls).toEqual([
      { command: 'get_snapshots', args: undefined },
      { command: 'get_history', args: { snapshotId: 'claude:default:weekly', range: '7d' } },
      { command: 'save_api_key', args: { adapterId: 'xai', key: 'key-1', teamId: 'team' } },
      { command: 'save_api_key', args: { adapterId: 'openrouter', key: 'key-2', teamId: null } },
      { command: 'remove_api_key', args: { adapterId: 'openrouter' } },
      { command: 'revoke_device', args: { deviceId: '00ff' } },
      {
        command: 'set_alert',
        args: { intent: { id: 'reset:claude:default:weekly', fireAt: '2026-10-16T00:00:00Z', enabled: true } },
      },
      { command: 'remove_alert', args: { id: 'reset:claude:default:weekly' } },
      { command: 'confirm_pairing', args: { accept: true } },
    ])
  })

  it('reads and saves the notification settings', async () => {
    const { source, calls } = setup((command) =>
      command === 'get_notifications' ? { limitBack: true, ranOut: true, lowAtPercent: 90 } : undefined,
    )
    expect(await source.notifications.get()).toEqual({ limitBack: true, ranOut: true, lowAtPercent: 90 })
    await source.notifications.set({ ranOut: false, lowAtPercent: 80 })
    expect(calls).toEqual([
      { command: 'get_notifications', args: undefined },
      { command: 'set_notifications', args: { notifications: { limitBack: true, ranOut: false, lowAtPercent: 80 } } },
    ])
  })
  it('refuses an empty key without calling the backend', async () => {
    const { source, calls } = setup()
    await expect(source.saveApiKey('openrouter', '   ')).rejects.toThrow('Enter a key first.')
    expect(calls).toHaveLength(0)
  })

  it('turns backend error strings into Errors', async () => {
    const { source } = setup(() => {
      throw 'That does not look like an API key.'
    })
    await expect(source.getAdapters()).rejects.toThrow('That does not look like an API key.')
  })

  it('listens once, relays change events, and stops when the last listener leaves', async () => {
    const { source, listenFn, stop, fire } = setup()
    const a = vi.fn()
    const b = vi.fn()
    const offA = source.subscribe(a)
    const offB = source.subscribe(b)
    expect(listenFn).toHaveBeenCalledTimes(1)
    await Promise.resolve()
    fire()
    expect(a).toHaveBeenCalledTimes(1)
    expect(b).toHaveBeenCalledTimes(1)
    offA()
    expect(stop).not.toHaveBeenCalled()
    offB()
    await Promise.resolve()
    await Promise.resolve()
    expect(stop).toHaveBeenCalledTimes(1)
  })

  it('tells subscribers after changes the backend does not announce', async () => {
    const { source } = setup()
    const listener = vi.fn()
    source.subscribe(listener)
    await source.installHook()
    await source.revokeDevice('00ff')
    await source.refresh()
    expect(listener).toHaveBeenCalledTimes(3)
  })

  it('tells the pairing screen when the window is closed, and stops listening on request', async () => {
    const stop = vi.fn()
    let handler: (() => void) | undefined
    const listenFn: ListenFn = vi.fn(async (event, h) => {
      expect(event).toBe(WINDOW_HIDDEN_EVENT)
      handler = h
      return stop
    })
    const source = new TauriSnapshotSource((async () => undefined) as InvokeFn, listenFn)
    const hidden = vi.fn()
    const off = source.pairing.onWindowHidden?.(hidden)
    await Promise.resolve()
    handler?.()
    expect(hidden).toHaveBeenCalledTimes(1)
    off?.()
    await Promise.resolve()
    await Promise.resolve()
    expect(stop).toHaveBeenCalledTimes(1)
  })
})
