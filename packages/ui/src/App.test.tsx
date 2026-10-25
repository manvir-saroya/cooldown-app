import { render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import type { Snapshot } from '@cooldown/snapshot-schema'
import { App } from './App'
import { SampleSnapshotSource } from './data/SampleSnapshotSource'

const NOW = Date.parse('2026-10-10T12:00:00Z')

function setup(options: { hash?: string; armed?: string[] } = {}) {
  location.hash = options.hash ?? '#/'
  const source = new SampleSnapshotSource({ now: () => NOW })
  const user = userEvent.setup()
  render(<App source={source} fixedNow={NOW} initialArmed={options.armed ?? []} />)
  return { user, source }
}

function card(name: RegExp) {
  return screen.getAllByRole('article').find((a) => name.test(a.getAttribute('aria-label') ?? a.textContent ?? ''))!
}

function panel(provider: string) {
  return within(screen.getByRole('article', { name: provider }))
}

describe('Overview', () => {
  it('shows one panel per provider with a verdict, every window with a live reset, and balances apart', async () => {
    setup()
    // Claude and Codex panels, plus the OpenRouter, DeepSeek and xAI balance tiles.
    expect(await screen.findAllByRole('article')).toHaveLength(5)
    const claude = panel('Claude')
    expect(claude.getByText('Ready')).toBeInTheDocument()
    expect(claude.getByText('Official')).toBeInTheDocument()
    expect(claude.getAllByRole('timer').map((t) => t.textContent)).toEqual(['2h 17m', '3d 5h'])
    expect(claude.getByRole('progressbar', { name: /Claude 5-hour session/ })).toHaveAttribute('aria-valuenow', '54')
    const codex = panel('Codex')
    expect(codex.getByText('Limit reached')).toBeInTheDocument()
    expect(within(screen.getByRole('region', { name: 'Balances' })).getAllByRole('article')).toHaveLength(3)
  })

  it('marks stale, degraded and unavailable honestly and keeps the last good value visible', async () => {
    setup()
    await screen.findAllByRole('article')
    const claude = panel('Claude')
    expect(claude.getByText('Stale')).toBeInTheDocument()
    expect(claude.getByText(/Weekly: Claude Code is not running/)).toBeInTheDocument()
    expect(claude.getByText('38')).toBeInTheDocument()

    const codex = panel('Codex')
    expect(codex.getByText('Degraded')).toBeInTheDocument()
    expect(codex.getByText('41')).toBeInTheDocument()

    const deepseek = within(card(/DeepSeek/))
    expect(deepseek.getByText('Unavailable')).toBeInTheDocument()
    expect(deepseek.getByText('$8.90')).toBeInTheDocument()
    expect(deepseek.getByText(/19 h ago/)).toBeInTheDocument()
  })

  it('leads with the soonest reset', async () => {
    setup()
    const heading = await screen.findByRole('heading', { name: /Codex 5-hour session is back in/ })
    expect(heading).toHaveTextContent('4m 40s')
  })

  it('arms and removes a bell from a row, named after its limit', async () => {
    const { user } = setup()
    await screen.findAllByRole('article')
    const claude = panel('Claude')
    const bell = claude.getByRole('button', { name: 'Notify me when Claude 5-hour session resets' })
    expect(bell).toHaveAttribute('aria-pressed', 'false')
    await user.click(bell)
    expect(bell).toHaveAttribute('aria-pressed', 'true')
    expect(bell).toHaveAccessibleName(/Bell set for when Claude 5-hour session resets/)
    await user.click(bell)
    expect(bell).toHaveAttribute('aria-pressed', 'false')
  })

  it('does not offer a bell for balances', async () => {
    setup()
    await screen.findAllByRole('article')
    expect(within(card(/OpenRouter/)).queryByRole('button', { name: /Notify me/ })).not.toBeInTheDocument()
  })

  it('does not call a limit window with no reset time yet a balance', async () => {
    const source = new SampleSnapshotSource({ now: () => NOW })
    const inner = source as unknown as { snapshots: Snapshot[] }
    inner.snapshots = inner.snapshots.map((s) =>
      s.provider === 'claude' && s.window === 'session_5h' ? { ...s, resetsAt: undefined } : s,
    )
    location.hash = '#/'
    const view = render(<App source={source} fixedNow={NOW} initialArmed={[]} />)
    await screen.findAllByRole('article')
    expect(panel('Claude').getByText('reset time not known yet')).toBeInTheDocument()
    view.unmount()

    // The full card on the provider screen says the same, and never "balance only".
    location.hash = '#/provider/claude/session_5h'
    render(<App source={source} fixedNow={NOW} initialArmed={[]} />)
    const card = within((await screen.findAllByRole('article'))[0]!)
    expect(card.getByText('time not known yet')).toBeInTheDocument()
    expect(card.getByText(/once the reset time is known/)).toBeInTheDocument()
    expect(card.queryByText(/balance only/)).not.toBeInTheDocument()
    expect(card.queryByText(/do not reset/)).not.toBeInTheDocument()
  })
})

describe('Agents and sources', () => {
  it('switching a source off hides it from the dashboard, and on brings it back', async () => {
    const { user, source } = setup({ hash: '#/settings' })
    const toggle = await screen.findByRole('switch', { name: 'Show Codex app-server on the dashboard' })
    expect(toggle).toBeChecked()
    await user.click(toggle)
    // The switch follows the source once the change is saved and the list is read again.
    await waitFor(() => expect(toggle).not.toBeChecked())
    expect((await source.getSnapshots()).some((s) => s.provider === 'codex')).toBe(false)

    location.hash = '#/'
    expect(await screen.findByRole('article', { name: 'Claude' })).toBeInTheDocument()
    expect(screen.queryByRole('article', { name: 'Codex' })).not.toBeInTheDocument()

    await source.setSourceEnabled('codex-app-server', true)
    expect(await screen.findByRole('article', { name: 'Codex' })).toBeInTheDocument()
  })

  it('lists terminal agents that are off until set up, with a hook to install', async () => {
    const { user } = setup({ hash: '#/settings' })
    const antigravity = await screen.findByRole('switch', { name: 'Show Antigravity CLI statusline on the dashboard' })
    expect(antigravity).not.toBeChecked()
    await user.click(antigravity)
    expect(await screen.findByText('Hook not installed')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Install hook' }))
    await user.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Install hook' }))
    expect(await screen.findByText('Hook installed')).toBeInTheDocument()
  })
})

describe('Notifications settings', () => {
  it('shows the three automatic notifications on, and saves a change', async () => {
    const { user, source } = setup({ hash: '#/settings' })
    const section = within(await screen.findByRole('region', { name: 'Notifications' }))
    const back = await section.findByRole('switch', { name: 'A limit is back' })
    expect(back).toBeChecked()
    expect(section.getByRole('switch', { name: 'A limit ran out' })).toBeChecked()
    expect(section.getByRole('switch', { name: 'Running low' })).toBeChecked()
    expect(section.getByRole('radio', { name: '90%' })).toHaveAttribute('aria-checked', 'true')

    await user.click(section.getByRole('radio', { name: '80%' }))
    await user.click(back)
    expect(await source.notifications.get()).toEqual({ limitBack: false, ranOut: true, lowAtPercent: 80 })
    await user.click(section.getByRole('switch', { name: 'Running low' }))
    expect(section.queryByRole('radio', { name: '80%' })).not.toBeInTheDocument()
    expect((await source.notifications.get()).lowAtPercent).toBeNull()
  })
})

describe('Alerts', () => {
  it('lists an armed bell with its fire time, can pause it, and shows the empty state after delete', async () => {
    const { user } = setup({ hash: '#/alerts', armed: ['codex:personal:session_5h'] })
    const toggle = await screen.findByRole('switch', { name: /Bell for Codex/ })
    expect(toggle).toBeChecked()
    const row = toggle.closest('li')!
    expect(within(row).getByText(/Rings/)).toBeInTheDocument()
    await user.click(toggle)
    expect(toggle).not.toBeChecked()
    expect(within(row).getByText(/Paused/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: /Delete bell for Codex/ }))
    expect(await screen.findByText('No bells set')).toBeInTheDocument()
  })

  it('offers windows to set a bell on, and arming one adds it to the list', async () => {
    const { user } = setup({ hash: '#/alerts' })
    expect(await screen.findByText('No bells set')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: /Set bell for Claude 5-hour session/ }))
    expect(await screen.findByRole('switch', { name: /Bell for Claude 5-hour session/ })).toBeChecked()
  })
})

describe('Provider detail', () => {
  it('shows history, reset schedule, source details and how the number is obtained', async () => {
    setup({ hash: '#/provider/claude/weekly' })
    expect(await screen.findByRole('heading', { name: 'Source details' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Reset schedule' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'Usage history' })).toBeInTheDocument()
    expect(screen.getByRole('heading', { name: 'How this number is obtained' })).toBeInTheDocument()
    expect(screen.getByRole('tab', { name: 'Weekly', selected: true })).toBeInTheDocument()
    expect(screen.getByText(/never reads your Claude login/)).toBeInTheDocument()
  })
})

describe('Settings', () => {
  it('masks keys, says they never leave the device, and revokes a paired device after confirming', async () => {
    const { user } = setup({ hash: '#/settings' })
    expect(await screen.findByText(/Keys stay on this device/)).toBeInTheDocument()
    const inputs = screen.getAllByLabelText(/API key|Management key/)
    expect(inputs.length).toBeGreaterThan(0)
    for (const input of inputs) expect(input).toHaveAttribute('type', 'password')

    await user.click(screen.getByRole('button', { name: 'Revoke Pixel 9' }))
    const dialog = await screen.findByRole('alertdialog').catch(() => screen.findByRole('dialog'))
    await user.click(within(dialog).getByRole('button', { name: 'Revoke Pixel 9' }))
    expect(screen.queryByRole('button', { name: 'Revoke Pixel 9' })).not.toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Revoke Galaxy Tab S9' })).toBeInTheDocument()
  })

  it('saves an xAI management key without a team ID, and sends the team ID when one is typed', async () => {
    const { user, source } = setup({ hash: '#/settings' })
    const save = vi.spyOn(source, 'saveApiKey')
    const keyField = await screen.findByLabelText('Management key')
    const form = keyField.closest('form')!
    const saveButton = within(form).getByRole('button', { name: 'Save key' })

    // Optional, and the text says when it is needed.
    const team = within(form).getByLabelText(/Team ID/)
    expect(team).not.toBeRequired()
    expect(within(form).getByText('(optional)')).toBeInTheDocument()
    expect(team).toHaveAccessibleDescription(/only for a key that covers your whole organization/)
    expect(saveButton).toBeDisabled()

    await user.type(keyField, 'xai-management-key')
    expect(saveButton).toBeEnabled()
    await user.click(saveButton)
    await waitFor(() => expect(save).toHaveBeenCalledTimes(1))
    expect(save).toHaveBeenLastCalledWith('xai-management', 'xai-management-key', undefined)

    await user.type(keyField, 'xai-org-key')
    await user.type(team, ' team_123 ')
    await user.click(saveButton)
    await waitFor(() => expect(save).toHaveBeenCalledTimes(2))
    expect(save).toHaveBeenLastCalledWith('xai-management', 'xai-org-key', { teamId: 'team_123' })
  })

  it('shows the hook status and confirms before uninstalling', async () => {
    const { user, source } = setup({ hash: '#/settings' })
    expect(await screen.findByText('Installed')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: 'Uninstall hook' }))
    const dialog = await screen.findByRole('dialog')
    await user.click(within(dialog).getByRole('button', { name: 'Uninstall hook' }))
    expect(await screen.findByText('Not installed')).toBeInTheDocument()
    expect((await source.getHookStatus()).installed).toBe(false)
  })

  it('lets the user choose a theme and applies it', async () => {
    const { user } = setup({ hash: '#/settings' })
    await user.click(await screen.findByRole('radio', { name: /Dark/ }))
    expect(document.documentElement).toHaveClass('dark')
    await user.click(screen.getByRole('radio', { name: /Light/ }))
    expect(document.documentElement).not.toHaveClass('dark')
  })
})

describe('Pairing', () => {
  it('shows a QR code, a manual code, and says it is local only', async () => {
    setup({ hash: '#/pairing' })
    expect(await screen.findByText(/CD1:/)).toBeInTheDocument()
    expect(document.querySelector('svg title')?.textContent).toMatch(/Pairing code/)
    expect(screen.getByText('Local network only')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: /Copy code/ })).toBeInTheDocument()
  })
})

describe('Navigation', () => {
  it('falls back to a not-found state with a way home', async () => {
    setup({ hash: '#/nope' })
    expect(await screen.findByText('That page does not exist')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: 'Go to overview' })).toBeInTheDocument()
  })
})

describe('Keyboard focus and landmarks', () => {
  it('returns focus to the button that opened a confirm dialog', async () => {
    const { user } = setup({ hash: '#/settings' })
    const open = await screen.findByRole('button', { name: 'Uninstall hook' })
    await user.click(open)
    await screen.findByRole('dialog')
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(open).toHaveFocus()
  })

  it('keeps keyboard focus on the refresh button while it works', async () => {
    const { user } = setup()
    await screen.findAllByRole('article')
    const refresh = screen.getByRole('button', { name: 'Refresh readings' })
    refresh.focus()
    await user.keyboard('{Enter}')
    expect(refresh).toHaveFocus()
    expect(refresh).not.toBeDisabled()
  })

  it('gives the provider window tabs a panel to point at', async () => {
    setup({ hash: '#/provider/claude/weekly' })
    const tab = await screen.findByRole('tab', { name: 'Weekly', selected: true })
    const panel = screen.getByRole('tabpanel')
    expect(tab).toHaveAttribute('aria-controls', panel.id)
    expect(within(panel).getByRole('heading', { name: 'Source details' })).toBeInTheDocument()
  })

  it('puts the desktop sidebar in a landmark', async () => {
    setup()
    await screen.findAllByRole('article')
    expect(screen.getByRole('complementary', { name: 'Sidebar' })).toBeInTheDocument()
  })

  it('does not claim the readings were refreshed before they have loaded', async () => {
    location.hash = '#/'
    const source = new SampleSnapshotSource({ now: () => NOW, latencyMs: 40 })
    render(<App source={source} fixedNow={NOW} />)
    expect(screen.getByText('Sample data')).toBeInTheDocument()
    expect(screen.queryByText(/Refreshed/)).not.toBeInTheDocument()
    expect(await screen.findByText(/Refreshed just now/)).toBeInTheDocument()
  })

  it('puts the overview back where it was scrolled to when the user comes back from a provider', async () => {
    const { user } = setup()
    await screen.findAllByRole('article')
    const scroller = document.getElementById('main-content')!.parentElement as HTMLElement
    let top = 640
    Object.defineProperty(scroller, 'scrollTop', { configurable: true, get: () => top, set: (v: number) => void (top = v) })
    const scrollTo = vi.spyOn(scroller, 'scrollTo').mockImplementation((arg) => {
      top = (arg as ScrollToOptions).top ?? 0
    })

    await user.click(screen.getAllByRole('link', { name: 'Claude' })[0]!)
    await screen.findByRole('heading', { name: 'Source details' })
    expect(scrollTo).toHaveBeenLastCalledWith({ top: 0 })

    location.hash = '#/'
    await screen.findAllByRole('article')
    await waitFor(() => expect(scrollTo).toHaveBeenLastCalledWith({ top: 640 }))
  })
})
