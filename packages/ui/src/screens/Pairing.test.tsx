/**
 * The PC's Pairing screen with a source that pairs for real: the typed code (the QR text), its Copy
 * button, and what happens to the secret when the window closes.
 */
import { act, render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import { App } from '../App'
import { SampleSnapshotSource } from '../data/SampleSnapshotSource'
import type { PairingControl } from '../data/SnapshotSource'
import type { PairingPayload } from '../data/types'

const NOW = Date.parse('2026-10-10T12:00:00Z')

/** A QR text in the real shape: `CD1:` and upper-case base32, about 130 characters. */
const code = (n: number) => `CD1:${'AF5BUTTQTPYILLCJ'.repeat(8)}${n}ZZ`

class PairingSource extends SampleSnapshotSource {
  readonly pairing: PairingControl
  readonly cancel = vi.fn(async () => {})
  readonly hiddenListeners = new Set<() => void>()
  made = 0

  constructor(private readonly manual: (n: number) => string = code) {
    super({ now: () => NOW })
    this.pairing = {
      getState: async () => ({ phase: 'waiting' }),
      confirm: async () => {},
      cancel: this.cancel,
      onWindowHidden: (listener) => {
        this.hiddenListeners.add(listener)
        return () => void this.hiddenListeners.delete(listener)
      },
    }
  }

  override async createPairingPayload(): Promise<PairingPayload> {
    this.made += 1
    const text = this.manual(this.made)
    return {
      uri: text,
      manualCode: text,
      host: '192.168.1.24',
      port: 47821,
      expiresAt: new Date(NOW + 2 * 60_000).toISOString(),
      fingerprint: '4F2A 91C7',
      placeholder: false,
    }
  }

  closeWindow() {
    for (const listener of [...this.hiddenListeners]) listener()
  }
}

function setup(source = new PairingSource()) {
  location.hash = '#/pairing'
  const user = userEvent.setup()
  render(<App source={source} fixedNow={NOW} />)
  return { user, source }
}

describe('PC pairing screen: the typed code', () => {
  it('shows the full QR text with a Copy button that puts it on the clipboard', async () => {
    const { user } = setup()
    expect(await screen.findByRole('heading', { name: 'Cannot scan? Copy the code' })).toBeInTheDocument()
    expect(await screen.findByText(code(1))).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: 'Copy code' }))
    expect(await navigator.clipboard.readText()).toBe(code(1))
    expect(await screen.findByRole('button', { name: 'Copied' })).toBeInTheDocument()
  })

  it('names the phone button that really exists', async () => {
    setup()
    const hint = await screen.findByText(/Paste code instead/)
    expect(hint).toHaveTextContent('choose “Paste code instead” and paste this')
    expect(screen.queryByText(/Enter code instead/)).not.toBeInTheDocument()
  })

  it('says so when the clipboard is not available', async () => {
    const { user } = setup()
    await screen.findByText(code(1))
    vi.spyOn(navigator.clipboard, 'writeText').mockRejectedValue(new Error('denied'))
    await user.click(screen.getByRole('button', { name: 'Copy code' }))
    expect(await screen.findByText('Could not copy')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Copy code' })).toBeInTheDocument()
  })

  it('hides the section for a source that has no typed code', async () => {
    setup(new PairingSource(() => ''))
    await screen.findByRole('button', { name: 'Get a new code' })
    await waitFor(() => expect(screen.queryByRole('heading', { name: /Cannot scan/ })).not.toBeInTheDocument())
  })
})

describe('PC pairing screen: the secret does not outlive the window', () => {
  it('drops the QR code and the typed code when the window is closed, then makes a new one when it comes back', async () => {
    const { source } = setup()
    expect(await screen.findByText(code(1))).toBeInTheDocument()
    expect(document.querySelector('svg title')).not.toBeNull()

    act(() => source.closeWindow())
    await waitFor(() => expect(screen.queryByText(code(1))).not.toBeInTheDocument())
    expect(document.querySelector('svg title')).toBeNull()
    expect(screen.getByRole('button', { name: 'Copy code' })).toBeDisabled()
    expect(screen.getByText('The code was hidden when the window closed.')).toBeInTheDocument()

    // The window is shown again: the screen asks for a fresh code by itself.
    act(() => void window.dispatchEvent(new Event('focus')))
    expect(await screen.findByText(code(2))).toBeInTheDocument()
    expect(source.made).toBe(2)
  })

  it('can also be brought back with the button', async () => {
    const { source, user } = setup()
    await screen.findByText(code(1))
    act(() => source.closeWindow())
    await user.click(await screen.findByRole('button', { name: 'Get a new code' }))
    expect(await screen.findByText(code(2))).toBeInTheDocument()
  })

  it('throws away a code that arrives after the window closed', async () => {
    const source = new PairingSource()
    let release!: () => void
    const gate = new Promise<void>((resolve) => (release = resolve))
    const make = source.createPairingPayload.bind(source)
    source.createPairingPayload = async () => {
      await gate
      return make()
    }
    setup(source)
    act(() => source.closeWindow())
    release()
    await waitFor(() => expect(source.made).toBe(1))
    await act(async () => {})
    expect(screen.queryByText(code(1))).not.toBeInTheDocument()
    expect(document.querySelector('svg title')).toBeNull()
  })

  it('stops listening and cancels the pairing window when the user leaves the screen', async () => {
    const { source } = setup()
    await screen.findByText(code(1))
    expect(source.hiddenListeners.size).toBe(1)
    location.hash = '#/'
    await waitFor(() => expect(source.hiddenListeners.size).toBe(0))
    expect(source.cancel).toHaveBeenCalled()
  })
})
