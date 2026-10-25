/**
 * The overview's lead: the soonest reset, drawn as a tide coming back in. The water's height is the
 * share of the window that has passed, so it is full when the limit returns. The countdown's digits
 * roll (the idea of NumberFlow, https://github.com/barvian/number-flow, done with Motion so the timer
 * stays plain text for assistive tech). Waves and rolling stand still under reduced motion.
 */
import * as m from 'motion/react-m'
import type { Snapshot } from '@cooldown/snapshot-schema'
import { providerInfo, windowTitle } from '@/lib/providers'
import { countdownSpoken, formatClock, formatCountdown, parseTime, percentUsed, windowLengthMs } from '@/lib/snapshot-view'
import { shallowEqual, useNowSelector, useToday } from '@/state/now'
import { BellButton } from './BellButton'

export function TideHero({ snapshot }: { snapshot: Snapshot }) {
  const today = useToday()
  const resetsAt = parseTime(snapshot.resetsAt)
  const length = windowLengthMs(snapshot.window)
  // Whole percents: the water moves about once a minute on a 5-hour window, not every second.
  const passed = useNowSelector((now) =>
    resetsAt === undefined || length === undefined ? 50 : Math.round(Math.min(1, Math.max(0, 1 - (resetsAt - now) / length)) * 100),
  )
  if (resetsAt === undefined) return null
  const name = `${providerInfo(snapshot.provider).name} ${windowTitle(snapshot.provider, snapshot.window).toLowerCase()}`
  const percent = percentUsed(snapshot)
  const full = percent !== undefined && percent >= 100
  // "Back" only fits a limit that is used up; anything else simply resets.
  const verb = full ? 'is back in' : 'resets in'

  return (
    <section
      aria-label="Next reset"
      className="relative isolate flex flex-col gap-5 overflow-hidden rounded-2xl border border-tide/30 bg-card p-5 sm:flex-row sm:items-end sm:justify-between sm:p-7"
    >
      <div
        aria-hidden="true"
        className="tide-water absolute inset-0 -z-10"
        style={{ transform: `translateY(${88 - passed * 0.7}%)` }}
      >
        <Wave className="tide-wave-slow absolute bottom-full left-0 h-4 w-[200%] opacity-60" />
        <Wave className="tide-wave absolute bottom-full left-0 h-3 w-[200%]" />
      </div>

      <div className="min-w-0">
        <p className="flex items-center gap-2 text-xs font-semibold tracking-wide text-tide-ink uppercase">
          <span aria-hidden="true" className="live-dot size-2 rounded-full bg-tide" />
          Next reset
        </p>
        <h2 className="mt-2 text-lg leading-snug font-semibold text-foreground sm:text-xl">
          {name} {verb} <RollingCountdown target={resetsAt} className="mt-1 flex w-fit text-5xl tracking-tight sm:text-6xl" />
        </h2>
        <p className="num mt-2 text-sm text-muted-foreground">
          At {formatClock(resetsAt, today)}
          {full ? '. The limit is reached now.' : percent !== undefined ? `. ${Math.round(percent)}% used.` : '.'}
          {length !== undefined && <> The tide is {passed}% back in.</>}
        </p>
      </div>
      <div className="sm:w-72 sm:shrink-0">
        <BellButton snapshot={snapshot} />
      </div>
    </section>
  )
}

/** A countdown whose digits roll into place. Each digit is plain text; a changed digit remounts and slides in. */
function RollingCountdown({ target, className }: { target: number; className?: string }) {
  const { text, spoken } = useNowSelector((now) => ({ text: formatCountdown(target, now), spoken: countdownSpoken(target, now) }), shallowEqual)
  const chars = [...text]
  return (
    <span role="timer" aria-live="off" aria-label={spoken} className={`num num-ticking overflow-hidden whitespace-pre ${className ?? ''}`}>
      {chars.map((c, i) => {
        // Keyed from the right, so "10m 0s" turning into "9m 59s" keeps the seconds in place.
        const slot = chars.length - i
        return /\d/.test(c) ? (
          <m.span
            key={`${slot}-${c}`}
            className="inline-block"
            initial={{ y: '-0.5em', opacity: 0 }}
            animate={{ y: 0, opacity: 1 }}
            transition={{ type: 'spring', duration: 0.45, bounce: 0.2 }}
          >
            {c}
          </m.span>
        ) : (
          <span key={slot}>{c}</span>
        )
      })}
    </span>
  )
}

const WAVE_WIDTH = 1200
const WAVE_PERIODS = 4

/** The wave outline is the same for every band, so it is built once. */
const WAVE_PATH = (() => {
  const step = WAVE_WIDTH / (WAVE_PERIODS * 2)
  let d = 'M0 10'
  for (let i = 0; i < WAVE_PERIODS * 2; i++) d += ` Q${step * i + step / 2} ${i % 2 ? 18 : 2} ${step * (i + 1)} 10`
  return `${d} L${WAVE_WIDTH} 20 L0 20 Z`
})()

/** One wave band, twice as wide as the hero, so drifting it by half loops without a seam. */
function Wave({ className }: { className?: string }) {
  return (
    <svg viewBox={`0 0 ${WAVE_WIDTH} 20`} preserveAspectRatio="none" className={className} aria-hidden="true">
      <path d={WAVE_PATH} style={{ fill: 'color-mix(in oklch, var(--tide) 14%, transparent)' }} />
    </svg>
  )
}
