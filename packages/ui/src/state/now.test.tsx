/**
 * The shared clock must not re-render the whole app every second. These tests drive a live clock with
 * fake timers and count renders: only components that show something that changed may render.
 */
import { act, render, screen } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { App } from '@/App'
import { SampleSnapshotSource } from '@/data/SampleSnapshotSource'
import { NowProvider, useNowSelector } from '@/state/now'

// Render counters. `pageTitle` runs once per AppShell render. Every clock hook runs once per render of
// each component that reads the clock, so the sum is "clock-driven component renders".
const counts = vi.hoisted(() => ({ clockReads: 0, shellRenders: 0 }))

vi.mock('@/state/now', async (importOriginal) => {
  const actual = await importOriginal<Record<string, unknown>>()
  const wrapped: Record<string, unknown> = { ...actual }
  for (const [name, value] of Object.entries(actual)) {
    if (/^use/.test(name) && typeof value === 'function') {
      wrapped[name] = (...args: unknown[]) => {
        counts.clockReads++
        return (value as (...a: unknown[]) => unknown)(...args)
      }
    }
  }
  return wrapped
})

vi.mock('@/shell/nav', async (importOriginal) => {
  const actual = await importOriginal<typeof import('@/shell/nav')>()
  return {
    ...actual,
    pageTitle: (...args: Parameters<typeof actual.pageTitle>) => {
      counts.shellRenders++
      return actual.pageTitle(...args)
    },
  }
})

const START = Date.parse('2026-10-10T12:00:00Z')

beforeEach(() => {
  // Only the timers the clock uses are faked, so promises and testing-library keep working.
  vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval', 'Date'] })
  vi.setSystemTime(START)
})

afterEach(() => {
  vi.useRealTimers()
})

async function advanceSeconds(seconds: number) {
  for (let i = 0; i < seconds; i++) {
    await act(async () => {
      vi.advanceTimersByTime(1000)
    })
  }
}

async function settle() {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0)
  })
}

describe('shared clock', () => {
  it('re-renders the shell only when a label it shows changes, not every second', async () => {
    const source = new SampleSnapshotSource({ now: () => Date.now() })
    location.hash = '#/'
    render(<App source={source} />)
    await settle()
    // Two provider panels (Claude, Codex) and three balance tiles.
    expect(screen.getAllByRole('article')).toHaveLength(5)

    counts.clockReads = 0
    counts.shellRenders = 0
    const seconds = 120
    await advanceSeconds(seconds)

    // The shell (title, sidebar, bottom bar, alerts) has nothing that changes each second. Its only live
    // label is "Refreshed ...", which turns over at 45 s and 90 s.
    expect(counts.shellRenders).toBeLessThanOrEqual(2)
    // Only countdowns below one hour tick each second; everything else renders when its text changes.
    // (Before the clock became a selector store this was about 45 component renders every second.)
    expect(counts.clockReads / seconds).toBeLessThan(6)
    expect(screen.getByText(/Refreshed 2 min ago/)).toBeInTheDocument()
  })

  it('still ticks a live countdown every second', async () => {
    const source = new SampleSnapshotSource({ now: () => Date.now() })
    location.hash = '#/'
    render(<App source={source} />)
    await settle()
    const heading = await screen.findByRole('heading', { name: /Codex 5-hour session is back in/ })
    expect(heading).toHaveTextContent('4m 40s')
    await advanceSeconds(3)
    expect(heading).toHaveTextContent('4m 37s')
  })

  it('rings an armed bell once when its time passes, with no per-second render in the alert state', async () => {
    const source = new SampleSnapshotSource({ now: () => Date.now() })
    location.hash = '#/alerts'
    render(<App source={source} initialArmed={['codex:personal:session_5h']} />)
    await settle()
    const toggle = screen.getByRole('switch', { name: /Bell for Codex/ })
    expect(toggle).toBeChecked()
    expect(screen.getByText(/Rings/)).toBeInTheDocument()

    await advanceSeconds(4 * 60 + 41)

    expect(await screen.findAllByText(/Codex 5-hour session is back/)).toHaveLength(1)
    expect(screen.getByText(/Rang at/)).toBeInTheDocument()
    expect(screen.getByRole('switch', { name: /Bell for Codex/ })).not.toBeChecked()
  })

  it('selector hooks render again only when the selected value changes', async () => {
    let renders = 0
    function Probe() {
      renders++
      const minute = useNowSelector((now) => Math.floor((now - START) / 60_000))
      return <p>minute {minute}</p>
    }
    render(
      <NowProvider>
        <Probe />
      </NowProvider>,
    )
    expect(screen.getByText('minute 0')).toBeInTheDocument()
    renders = 0
    await advanceSeconds(59)
    expect(renders).toBe(0)
    await advanceSeconds(1)
    expect(renders).toBe(1)
    expect(screen.getByText('minute 1')).toBeInTheDocument()
  })

  it('a selector on the whole time gives every second to the components that ask for it', async () => {
    function Seconds() {
      return <p>{Math.round((useNowSelector((now) => now) - START) / 1000)} s</p>
    }
    render(
      <NowProvider>
        <Seconds />
      </NowProvider>,
    )
    await advanceSeconds(5)
    expect(screen.getByText('5 s')).toBeInTheDocument()
  })

  it('stops the timer when nothing listens and when the component unmounts', async () => {
    const clear = vi.spyOn(globalThis, 'clearInterval')
    const { unmount } = render(
      <NowProvider>
        <p>idle</p>
      </NowProvider>,
    )
    // Nothing subscribed: no interval was ever started.
    expect(vi.getTimerCount()).toBe(0)
    unmount()

    function Seconds() {
      return <p>{useNowSelector((now) => now)}</p>
    }
    const second = render(
      <NowProvider>
        <Seconds />
      </NowProvider>,
    )
    expect(vi.getTimerCount()).toBe(1)
    second.unmount()
    expect(vi.getTimerCount()).toBe(0)
    expect(clear).toHaveBeenCalled()
  })
})
