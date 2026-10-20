/**
 * Phone mode: the same <App> with a source that provides `phone`. PC mode is covered by App.test.tsx.
 */
import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import type { AlertIntent, Snapshot } from '@cooldown/snapshot-schema'
import { App } from './App'
import { SampleSnapshotSource } from './data/SampleSnapshotSource'
import type { AlertBackend, PhoneControl, WidgetControl } from './data/SnapshotSource'
import type { PairedPc, PhonePermissions, WidgetSettings } from './data/types'

const NOW = Date.parse('2026-10-10T12:00:00Z')
const MIN = 60_000

const ALL_ALLOWED: PhonePermissions = { notifications: true, exactAlarms: true, localNetwork: true }

function pc(overrides: Partial<PairedPc> = {}): PairedPc {
  return {
    id: 'pc-1',
    name: 'Desk PC',
    pairedAt: new Date(NOW - 3 * 86_400_000).toISOString(),
    lastSeenAt: new Date(NOW - 4 * MIN).toISOString(),
    connected: true,
    ...overrides,
  }
}

function deferred<T = void>() {
  let resolve!: (value: T) => void
  let reject!: (reason: unknown) => void
  const promise = new Promise<T>((res, rej) => {
    resolve = res
    reject = rej
  })
  return { promise, resolve, reject }
}

type Setup = {
  hash?: string
  pcs?: PairedPc[]
  permissions?: PhonePermissions
  /** Override pieces of the phone control. */
  phone?: Partial<PhoneControl>
  /** `[]` gives the phone no readings yet. */
  noSnapshots?: boolean
  /** Give the phone a home-screen widget. */
  widget?: { settings?: Partial<WidgetSettings>; control?: Partial<WidgetControl> }
}

/** A sample source that also behaves like a phone: it has a PC list, permissions and a bell store on the "PC". */
class FakePhoneSource extends SampleSnapshotSource {
  readonly phone: PhoneControl
  readonly alerts: AlertBackend
  readonly setAlert = vi.fn(async (intent: AlertIntent) => {
    this.bells = [...this.bells.filter((b) => b.id !== intent.id), intent]
  })
  readonly getHistorySpy = vi.fn()
  pcs: PairedPc[]
  permissions: PhonePermissions
  widgetSettings: WidgetSettings = { windows: null, canPin: true }
  bells: AlertIntent[] = []
  private readonly watchers = new Set<() => void>()

  constructor(private readonly options: Setup) {
    super({ now: () => NOW })
    this.pcs = options.pcs ?? [pc()]
    this.permissions = options.permissions ?? ALL_ALLOWED
    this.alerts = {
      list: async () => this.bells.map((b) => ({ ...b })),
      set: this.setAlert,
      remove: async (id) => {
        this.bells = this.bells.map((b) => (b.id === id ? { ...b, enabled: false } : b))
      },
    }
    this.phone = {
      scanQr: vi.fn(async () => 'CD1:scanned'),
      beginPairing: vi.fn(async () => ({ code: '482193', pcName: 'Desk PC' })),
      finishPairing: vi.fn(async () => undefined),
      pairedPcs: vi.fn(async () => this.pcs.map((p) => ({ ...p }))),
      unpair: vi.fn(async (id: string) => {
        this.pcs = this.pcs.filter((p) => p.id !== id)
        this.notify()
      }),
      reconnect: vi.fn(async () => {
        this.pcs = this.pcs.map((p) => ({ ...p, connected: true }))
        this.notify()
      }),
      permissions: vi.fn(async () => ({ ...this.permissions })),
      askPermissions: vi.fn(async () => ({ ...this.permissions })),
      ...(options.widget ? { widget: this.makeWidget(options.widget) } : {}),
      ...options.phone,
    }
  }

  private makeWidget(options: NonNullable<Setup['widget']>): WidgetControl {
    this.widgetSettings = { ...this.widgetSettings, ...options.settings }
    const current = (): WidgetSettings => ({
      ...this.widgetSettings,
      windows: this.widgetSettings.windows && [...this.widgetSettings.windows],
    })
    return {
      getSettings: vi.fn(async () => current()),
      setWindows: vi.fn(async (windows: string[] | null) => {
        this.widgetSettings = { ...this.widgetSettings, windows }
        return current()
      }),
      pin: vi.fn(async () => true),
      ...options.control,
    }
  }

  override subscribe(listener: () => void): () => void {
    this.watchers.add(listener)
    return () => void this.watchers.delete(listener)
  }

  notify() {
    for (const w of [...this.watchers]) w()
  }

  override getSnapshots(): Promise<Snapshot[]> {
    return this.options.noSnapshots ? Promise.resolve([]) : super.getSnapshots()
  }

  override getHistory(...args: Parameters<SampleSnapshotSource['getHistory']>) {
    this.getHistorySpy(...args)
    return super.getHistory(...args)
  }
}

function setup(options: Setup = {}) {
  location.hash = options.hash ?? '#/'
  const source = new FakePhoneSource(options)
  const user = userEvent.setup()
  render(<App source={source} fixedNow={NOW} />)
  return { user, source }
}

describe('Phone pairing', () => {
  it('scans a code, shows the six digits, waits for the PC, then says it is paired', async () => {
    const confirmed = deferred()
    const { user, source } = setup({
      hash: '#/pairing',
      pcs: [],
      phone: { finishPairing: vi.fn(() => confirmed.promise) },
    })
    expect(screen.getByRole('heading', { level: 1, name: 'Pair with your PC' })).toBeInTheDocument()

    await user.click(await screen.findByRole('button', { name: 'Scan code' }))
    expect(await screen.findByRole('heading', { name: 'Confirm it on your PC' })).toBeInTheDocument()
    expect(source.phone.beginPairing).toHaveBeenCalledWith('CD1:scanned')
    expect(screen.getByLabelText('Code 4 8 2 1 9 3')).toHaveTextContent('482 193')
    expect(screen.getByText('Waiting for your PC…')).toBeInTheDocument()
    expect(screen.queryByText(/is paired|Paired with/)).not.toBeInTheDocument()

    source.pcs = [pc()]
    confirmed.resolve()
    expect(await screen.findByRole('heading', { name: /Paired with Desk PC/ })).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'See your limits' })).toHaveAttribute('href', '#/')
  })

  it('goes quietly back to the start when the camera is cancelled', async () => {
    const { user, source } = setup({
      hash: '#/pairing',
      pcs: [],
      phone: {
        scanQr: vi.fn(async () => {
          throw new Error('cancelled')
        }),
      },
    })
    await user.click(await screen.findByRole('button', { name: 'Scan code' }))
    expect(await screen.findByRole('button', { name: 'Scan code' })).toBeEnabled()
    expect(source.phone.beginPairing).not.toHaveBeenCalled()
    expect(screen.queryByText('Could not pair')).not.toBeInTheDocument()
  })

  it('pairs from pasted text', async () => {
    const { user, source } = setup({ hash: '#/pairing', pcs: [] })
    await user.click(await screen.findByRole('button', { name: 'Paste code instead' }))
    const pair = screen.getByRole('button', { name: 'Pair' })
    expect(pair).toBeDisabled()
    await user.type(screen.getByLabelText('Code from your PC'), '  CD1:abc123  ')
    await user.click(pair)
    expect(source.phone.scanQr).not.toHaveBeenCalled()
    await waitFor(() => expect(source.phone.beginPairing).toHaveBeenCalledWith('CD1:abc123'))
    expect(await screen.findByRole('heading', { name: /Paired with Desk PC/ })).toBeInTheDocument()
  })

  it('shows why pairing failed and lets the user try again', async () => {
    const begin = vi
      .fn<PhoneControl['beginPairing']>()
      .mockRejectedValueOnce('That code has expired.')
      .mockResolvedValue({ code: '111222', pcName: 'Desk PC' })
    const { user } = setup({
      hash: '#/pairing',
      pcs: [],
      phone: { beginPairing: begin, finishPairing: () => new Promise<void>(() => {}) },
    })
    await user.click(await screen.findByRole('button', { name: 'Scan code' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('That code has expired.')
    await user.click(screen.getByRole('button', { name: 'Try again' }))
    await user.click(await screen.findByRole('button', { name: 'Scan code' }))
    expect(await screen.findByLabelText('Code 1 1 1 2 2 2')).toBeInTheDocument()
  })

  it('shows the reason when the PC does not confirm', async () => {
    const { user } = setup({
      hash: '#/pairing',
      pcs: [],
      phone: { finishPairing: vi.fn().mockRejectedValue(new Error('Your PC said the codes did not match.')) },
    })
    await user.click(await screen.findByRole('button', { name: 'Scan code' }))
    expect(await screen.findByRole('alert')).toHaveTextContent('Your PC said the codes did not match.')
  })

  it('ignores a late answer after the user cancels', async () => {
    const confirmed = deferred()
    const { user } = setup({ hash: '#/pairing', pcs: [], phone: { finishPairing: vi.fn(() => confirmed.promise) } })
    await user.click(await screen.findByRole('button', { name: 'Scan code' }))
    await screen.findByRole('heading', { name: 'Confirm it on your PC' })
    await user.click(screen.getByRole('button', { name: 'Cancel' }))
    confirmed.resolve()
    expect(await screen.findByRole('button', { name: 'Scan code' })).toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: /Paired with/ })).not.toBeInTheDocument()
  })

  it('does not show the PC-side QR code or manual code', async () => {
    setup({ hash: '#/pairing' })
    expect(await screen.findByText('Local network only')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: /Copy code|Get a new code/ })).not.toBeInTheDocument()
    expect(document.querySelector('svg title')).toBeNull()
  })
})

describe('Phone settings', () => {
  it('hides API keys and the Claude hook, and says the keys stay on the PC', async () => {
    setup({ hash: '#/settings' })
    expect(await screen.findByText('Keys stay on your PC.')).toBeInTheDocument()
    expect(screen.queryByLabelText(/API key|Management key/)).not.toBeInTheDocument()
    expect(screen.queryByText(/Claude Code statusline hook/)).not.toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: 'Adapters' })).not.toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Appearance' })).toBeInTheDocument()
  })

  it('shows the paired PC with its connection state', async () => {
    setup({ hash: '#/settings', pcs: [pc({ connected: true }), pc({ id: 'pc-2', name: 'Laptop', connected: false, lastSeenAt: undefined })] })
    const desk = (await screen.findByText('Desk PC')).closest('li')!
    expect(within(desk).getByText('Connected')).toBeInTheDocument()
    expect(within(desk).getByText(/Last synced 4 min ago/)).toBeInTheDocument()
    const laptop = screen.getByText('Laptop').closest('li')!
    expect(within(laptop).getByText('Not connected')).toBeInTheDocument()
    expect(within(laptop).getByText(/Never synced/)).toBeInTheDocument()
  })

  it('confirms before unpairing, then forgets the PC', async () => {
    const { user, source } = setup({ hash: '#/settings' })
    await user.click(await screen.findByRole('button', { name: 'Unpair Desk PC' }))
    const dialog = await screen.findByRole('dialog')
    expect(source.phone.unpair).not.toHaveBeenCalled()
    await user.click(within(dialog).getByRole('button', { name: 'Unpair Desk PC' }))
    expect(source.phone.unpair).toHaveBeenCalledWith('pc-1')
    const empty = (await screen.findByText('No PC is paired.', { exact: false })).closest('div')!
    expect(within(empty).getByRole('link', { name: 'Pair with your PC' })).toHaveAttribute('href', '#/pairing')
  })

  it('keeps the PC when the user backs out of the unpair dialog', async () => {
    const { user, source } = setup({ hash: '#/settings' })
    await user.click(await screen.findByRole('button', { name: 'Unpair Desk PC' }))
    await user.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Keep pairing' }))
    expect(source.phone.unpair).not.toHaveBeenCalled()
    expect(screen.getByText('Desk PC')).toBeInTheDocument()
  })

  it('reconnects on request and reflects the new state', async () => {
    const { user, source } = setup({ hash: '#/settings', pcs: [pc({ connected: false })] })
    expect(await screen.findByText('Not connected')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Reconnect' }))
    expect(source.phone.reconnect).toHaveBeenCalledTimes(1)
    expect(await screen.findByText('Connected')).toBeInTheDocument()
  })

  it('says why a reconnect failed and keeps the old state', async () => {
    const { user } = setup({
      hash: '#/settings',
      pcs: [pc({ connected: false })],
      phone: { reconnect: vi.fn().mockRejectedValue('Your PC did not answer.') },
    })
    await user.click(await screen.findByRole('button', { name: 'Reconnect' }))
    expect(await screen.findByText('Your PC did not answer.')).toBeInTheDocument()
    expect(screen.getByText('Not connected')).toBeInTheDocument()
  })

  it('explains each missing permission and asks for them with one button', async () => {
    const { user, source } = setup({
      hash: '#/settings',
      permissions: { notifications: false, exactAlarms: false, localNetwork: true },
      phone: {},
    })
    const alarms = (await screen.findByText('Exact alarms')).closest('li')!
    expect(within(alarms).getByText('Off')).toBeInTheDocument()
    expect(within(alarms).getByText(/minutes late/)).toBeInTheDocument()
    const permissions = screen.getByRole('region', { name: 'Permissions' })
    const notifications = within(permissions).getByText('Notifications').closest('li')!
    expect(within(notifications).getByText('Off')).toBeInTheDocument()
    const network = screen.getByText('Local network').closest('li')!
    expect(within(network).getByText('Allowed')).toBeInTheDocument()

    vi.mocked(source.phone.askPermissions).mockImplementation(async () => {
      source.permissions = ALL_ALLOWED
      return ALL_ALLOWED
    })
    await user.click(screen.getByRole('button', { name: 'Grant permissions' }))
    expect(source.phone.askPermissions).toHaveBeenCalledTimes(1)
    expect(await screen.findByText('Everything Cooldown needs is allowed.')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Grant permissions' })).not.toBeInTheDocument()
  })

  it('reads the permissions again when the user comes back from Android settings', async () => {
    const { source } = setup({ hash: '#/settings', permissions: { ...ALL_ALLOWED, exactAlarms: false } })
    expect(await screen.findByRole('button', { name: 'Grant permissions' })).toBeInTheDocument()
    source.permissions = ALL_ALLOWED
    window.dispatchEvent(new Event('focus'))
    expect(await screen.findByText('Everything Cooldown needs is allowed.')).toBeInTheDocument()
  })
})

describe('Phone widget settings', () => {
  const ALL = [
    'claude:personal:session_5h',
    'claude:personal:weekly',
    'codex:personal:session_5h',
    'codex:personal:weekly',
    'openrouter:personal:balance',
    'deepseek:personal:balance',
    'xai:team:balance',
  ]

  it('has no widget section on a phone that has no widget', async () => {
    setup({ hash: '#/settings' })
    await screen.findByText('Keys stay on your PC.')
    expect(screen.queryByRole('heading', { name: 'Home-screen widget' })).not.toBeInTheDocument()
  })

  it('shows every window by default, with no list to fill in', async () => {
    setup({ hash: '#/settings', widget: {} })
    const everything = await screen.findByRole('switch', { name: /Show every window/ })
    expect(everything).toBeChecked()
    expect(screen.queryByRole('checkbox')).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Add widget to home screen' })).toBeEnabled()
  })

  it('lists the windows in the order of the app when "every window" is turned off', async () => {
    const { user, source } = setup({ hash: '#/settings', widget: {} })
    await user.click(await screen.findByRole('switch', { name: /Show every window/ }))
    expect(source.phone.widget?.setWindows).toHaveBeenCalledWith(ALL)

    const boxes = await screen.findAllByRole('checkbox')
    expect(boxes.map((b) => b.closest('label')?.textContent)).toEqual([
      'Claude · 5-hour session',
      'Claude · Weekly',
      'Codex · 5-hour session',
      'Codex · Weekly',
      'OpenRouter · Credit balance',
      'DeepSeek · Top-up balance',
      'xAI · Prepaid balance',
    ])
    expect(boxes.every((b) => (b as HTMLInputElement).checked)).toBe(true)
  })

  it('saves each tick, keeps the app order, and never lets the last window go', async () => {
    const { user, source } = setup({
      hash: '#/settings',
      widget: { settings: { windows: ['codex:personal:weekly', 'claude:personal:session_5h'] } },
    })
    expect(await screen.findByRole('switch', { name: /Show every window/ })).not.toBeChecked()
    const boxes = await screen.findAllByRole('checkbox')
    expect(boxes.filter((b) => (b as HTMLInputElement).checked)).toHaveLength(2)

    await user.click(screen.getByRole('checkbox', { name: 'OpenRouter · Credit balance' }))
    expect(source.phone.widget?.setWindows).toHaveBeenLastCalledWith([
      'claude:personal:session_5h',
      'codex:personal:weekly',
      'openrouter:personal:balance',
    ])

    await user.click(screen.getByRole('checkbox', { name: 'Codex · Weekly' }))
    await user.click(screen.getByRole('checkbox', { name: 'OpenRouter · Credit balance' }))
    await waitFor(() => expect(screen.getByRole('checkbox', { name: 'Claude · 5-hour session' })).toBeDisabled())
    expect(source.phone.widget?.setWindows).toHaveBeenLastCalledWith(['claude:personal:session_5h'])
    expect(screen.getByText('The widget needs at least one window.')).toBeInTheDocument()
  })

  it('goes back to every window, including ones the PC adds later', async () => {
    const { user, source } = setup({
      hash: '#/settings',
      widget: { settings: { windows: ['claude:personal:weekly'] } },
    })
    await user.click(await screen.findByRole('switch', { name: /Show every window/ }))
    expect(source.phone.widget?.setWindows).toHaveBeenLastCalledWith(null)
    await waitFor(() => expect(screen.queryByRole('checkbox')).not.toBeInTheDocument())
  })

  it('hides the add button when the launcher cannot place widgets, and says how to add it', async () => {
    setup({ hash: '#/settings', widget: { settings: { canPin: false } } })
    await screen.findByRole('switch', { name: /Show every window/ })
    expect(screen.queryByRole('button', { name: 'Add widget to home screen' })).not.toBeInTheDocument()
    expect(screen.getByText(/press and hold the home screen, choose Widgets, then Cooldown/i)).toBeInTheDocument()
  })

  it('asks the launcher to place the widget', async () => {
    const { user, source } = setup({ hash: '#/settings', widget: {} })
    await user.click(await screen.findByRole('button', { name: 'Add widget to home screen' }))
    expect(source.phone.widget?.pin).toHaveBeenCalledTimes(1)
    expect(await screen.findByText('Choose a spot for the widget')).toBeInTheDocument()
  })

  it('tells the user when the launcher turned the request down', async () => {
    const { user } = setup({ hash: '#/settings', widget: { control: { pin: vi.fn(async () => false) } } })
    await user.click(await screen.findByRole('button', { name: 'Add widget to home screen' }))
    expect(await screen.findByText('Your launcher cannot add it from here')).toBeInTheDocument()
  })

  it('says when the choice could not be saved and keeps the old one', async () => {
    const { user } = setup({
      hash: '#/settings',
      widget: { control: { setWindows: vi.fn().mockRejectedValue('The widget is not available.') } },
    })
    await user.click(await screen.findByRole('switch', { name: /Show every window/ }))
    expect(await screen.findByText('Could not change the widget')).toBeInTheDocument()
    expect(screen.getByText('The widget is not available.')).toBeInTheDocument()
    expect(screen.getByRole('switch', { name: /Show every window/ })).toBeChecked()
  })

  it('offers no list before anything has synced', async () => {
    setup({ hash: '#/settings', widget: {}, noSnapshots: true })
    const everything = await screen.findByRole('switch', { name: /Show every window/ })
    expect(everything).toBeDisabled()
    expect(screen.getByText(/appear here after your PC has synced/)).toBeInTheDocument()
  })
})

describe('Phone overview, alerts and provider detail', () => {
  it('asks to pair when the phone has no readings and no PC', async () => {
    setup({ pcs: [], noSnapshots: true })
    const empty = (await screen.findByText('No limits yet')).closest('div')!.parentElement!
    expect(within(empty).getByRole('link', { name: 'Pair with your PC' })).toHaveAttribute('href', '#/pairing')
  })

  it('says it is waiting when a PC is paired but nothing has synced', async () => {
    setup({ noSnapshots: true })
    expect(await screen.findByText('Waiting for your PC')).toBeInTheDocument()
  })

  it('stays quiet while the PC is connected', async () => {
    setup()
    await screen.findAllByRole('article')
    expect(screen.queryByRole('status', { name: 'Connection to your PC' })).not.toBeInTheDocument()
  })

  it('warns that readings are old when the PC is not connected, and can reconnect', async () => {
    const { user, source } = setup({ pcs: [pc({ connected: false })] })
    const note = await screen.findByRole('status', { name: 'Connection to your PC' })
    expect(note).toHaveTextContent('Desk PC is not connected.')
    expect(note).toHaveTextContent('4 min ago')
    await user.click(within(note).getByRole('button', { name: 'Reconnect' }))
    expect(source.phone.reconnect).toHaveBeenCalled()
    await waitFor(() => expect(screen.queryByRole('status', { name: 'Connection to your PC' })).not.toBeInTheDocument())
  })

  it('titles the pair tab for a phone', async () => {
    setup({ hash: '#/pairing' })
    expect(await screen.findByRole('heading', { level: 1, name: 'Pair with your PC' })).toBeInTheDocument()
    expect(document.title).toBe('Pair with your PC · Cooldown')
  })

  it('sends a bell to the PC and does not offer to delete one', async () => {
    const { user, source } = setup({ hash: '#/alerts' })
    expect(await screen.findByText('Bells are kept on your PC.')).toBeInTheDocument()
    await user.click(await screen.findByRole('button', { name: /Set bell for Claude 5-hour session/ }))
    await waitFor(() => expect(source.setAlert).toHaveBeenCalledTimes(1))
    expect(source.setAlert.mock.calls[0]?.[0]).toMatchObject({ id: 'reset:claude:personal:session_5h', enabled: true })
    expect(await screen.findByRole('switch', { name: /Bell for Claude 5-hour session/ })).toBeChecked()
    expect(screen.queryByRole('button', { name: /Delete bell/ })).not.toBeInTheDocument()
  })

  it('reminds the user on the alerts screen when bells could be late or silent', async () => {
    const { user, source } = setup({ hash: '#/alerts', permissions: { ...ALL_ALLOWED, exactAlarms: false } })
    const nudge = await screen.findByRole('region', { name: 'Bells may be late or silent' })
    expect(nudge).toHaveTextContent('exact alarms')
    await user.click(within(nudge).getByRole('button', { name: 'Grant permissions' }))
    expect(source.phone.askPermissions).toHaveBeenCalled()
  })

  it('shows no reminder when everything is allowed', async () => {
    setup({ hash: '#/alerts' })
    await screen.findByText('Bells are kept on your PC.')
    expect(screen.queryByRole('region', { name: /Bells may be late/ })).not.toBeInTheDocument()
  })

  it('says history lives on the PC instead of drawing a chart', async () => {
    const { source } = setup({ hash: '#/provider/claude/weekly' })
    expect(await screen.findByText(/History is kept on your PC/)).toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: 'Usage history' })).not.toBeInTheDocument()
    expect(screen.queryByText('Earlier resets')).not.toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Reset schedule' })).toBeInTheDocument()
    expect(source.getHistorySpy).not.toHaveBeenCalled()
  })
})

describe('Phone bells', () => {
  it('says a synced bell rang once its time has passed', async () => {
    location.hash = '#/'
    const source = new FakePhoneSource({})
    // The PC's list still holds a bell whose time passed (the phone has not heard from the PC since).
    source.bells = [{ id: 'reset:claude:personal:session_5h', fireAt: new Date(NOW - 60_000).toISOString(), enabled: true }]
    render(<App source={source} fixedNow={NOW} />)
    expect(await screen.findByText(/Bell rang at/)).toBeInTheDocument()
    expect(screen.queryByText(/Bell rings at/)).not.toBeInTheDocument()
  })
})

describe('Phone header', () => {
  it('says the phone is not paired yet instead of a refresh time', async () => {
    setup({ pcs: [], noSnapshots: true })
    expect(await screen.findByText('Not paired yet')).toBeInTheDocument()
    expect(screen.queryByText(/Refreshed/)).not.toBeInTheDocument()
  })

  it('shows the source and the refresh time once a PC is paired', async () => {
    const { source } = setup()
    expect(await screen.findByText(source.label)).toBeInTheDocument()
    expect(await screen.findByText(/Refreshed/)).toBeInTheDocument()
    expect(screen.queryByText('Not paired yet')).not.toBeInTheDocument()
  })
})

describe('PC mode is untouched', () => {
  it('keeps keys, the hook and the QR code for a source without phone', async () => {
    location.hash = '#/settings'
    render(<App source={new SampleSnapshotSource({ now: () => NOW })} fixedNow={NOW} />)
    expect(await screen.findByText(/Keys stay on this device/)).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Claude Code statusline hook' })).toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: 'Permissions' })).not.toBeInTheDocument()
    expect(screen.queryByRole('heading', { name: 'Your PC' })).not.toBeInTheDocument()
  })

  it('keeps the QR code and the PC titles for a source without phone', async () => {
    location.hash = '#/pairing'
    render(<App source={new SampleSnapshotSource({ now: () => NOW })} fixedNow={NOW} />)
    expect(await screen.findByText(/CD1:/)).toBeInTheDocument()
    expect(screen.getByRole('heading', { level: 1, name: 'Pair a device' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Scan code' })).not.toBeInTheDocument()
  })
})
