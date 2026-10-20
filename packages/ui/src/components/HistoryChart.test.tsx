import { render, screen } from '@testing-library/react'
import { act } from 'react'
import { toast } from 'sonner'
import { describe, expect, it } from 'vitest'
import { Toaster } from '@/components/ui/sonner'
import { ThemeProvider } from '@/state/theme'
import { evenTicks, tickLabel } from './HistoryChart'

const HOUR = 3_600_000
const START = Date.parse('2026-10-18T14:00:00Z')

describe('x-axis tick labels', () => {
  it('tells the first and last tick of a 24-hour chart apart', () => {
    const ticks = evenTicks(START, START + 24 * HOUR, 5)
    const labels = ticks.map((t) => tickLabel('24h', t, ticks[0]))
    expect(labels).toHaveLength(5)
    expect(labels[0]).not.toBe(labels[4])
    // The first tick is a plain time; a tick on a later day adds which day.
    expect(labels[0]).toBe(tickLabel('24h', ticks[0]!))
    expect(labels[4]!.length).toBeGreaterThan(labels[0]!.length)
    // No two ticks share a label.
    expect(new Set(labels).size).toBe(5)
  })

  it('keeps a same-day range as plain times', () => {
    const ticks = evenTicks(START, START + 4 * HOUR, 5)
    const labels = ticks.map((t) => tickLabel('24h', t, ticks[0]))
    expect(new Set(labels).size).toBe(5)
    // Nothing falls on another day, so no label needs one.
    ticks.forEach((t, i) => expect(labels[i]).toBe(tickLabel('24h', t)))
  })

  it('tells the ends of a 7-day chart apart, and keeps 30 days as dates', () => {
    const week = evenTicks(START, START + 7 * 24 * HOUR, 5).map((t) => tickLabel('7d', t))
    expect(week[0]).not.toBe(week[4])
    expect(new Set(week).size).toBe(5)
    const month = evenTicks(START, START + 30 * 24 * HOUR, 5).map((t) => tickLabel('30d', t))
    expect(new Set(month).size).toBe(5)
  })
})

describe('toast position', () => {
  it('keeps a top toast clear of the notch and status bar', async () => {
    render(
      <ThemeProvider>
        <Toaster position="top-center" />
      </ThemeProvider>,
    )
    act(() => void toast('Saved'))
    await screen.findByText('Saved')
    const toaster = document.querySelector<HTMLElement>('[data-sonner-toaster]')!
    expect(toaster.style.getPropertyValue('--offset-top')).toContain('env(safe-area-inset-top')
    expect(toaster.style.getPropertyValue('--mobile-offset-top')).toContain('env(safe-area-inset-top')
    // Only the top moved: the sides keep sonner's own spacing.
    expect(toaster.style.getPropertyValue('--mobile-offset-left')).toBe('16px')
  })
})
