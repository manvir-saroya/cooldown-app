/**
 * One shared clock, read through selectors.
 *
 * The clock is an external store (`useSyncExternalStore`), not React context state. A context value that
 * changes every second re-renders every consumer every second, including the app shell. Here a component
 * says what it needs from the time (`useNowSelector((now) => formatAge(now - at))`) and React re-renders
 * it only when that answer changes. A "2 min ago" label renders about once a minute, a clock label on
 * the day, a countdown above one hour once a minute, and only a countdown below one hour every second.
 *
 * The live clock ticks once a second only while something listens and the tab is visible, and it catches
 * up immediately when the tab comes back. Pass `fixed` in tests and screenshots for a frozen clock.
 */
import { createContext, useContext, useMemo, useRef, useSyncExternalStore, type ReactNode } from 'react'
import { formatAge, startOfDay } from '@/lib/snapshot-view'

export interface Clock {
  /** Epoch milliseconds of the latest tick. Stable between ticks, so it is safe as a store snapshot. */
  get(): number
  /** The time right now, for event handlers and effects. Never stale, unlike `get`. */
  read(): number
  subscribe(listener: () => void): () => void
}

/** A clock that never moves. */
export function createFixedClock(now: number): Clock {
  return { get: () => now, read: () => now, subscribe: () => () => {} }
}

/** The real clock: one interval for the whole app, running only while a listener exists and the tab is visible. */
export function createLiveClock(): Clock {
  let now = Date.now()
  let timer: ReturnType<typeof setInterval> | undefined
  const listeners = new Set<() => void>()

  const tick = () => {
    const next = Date.now()
    if (next === now) return
    now = next
    for (const listener of [...listeners]) listener()
  }
  const run = () => {
    if (timer !== undefined) return
    tick()
    timer = setInterval(tick, 1000)
  }
  const pause = () => {
    if (timer !== undefined) clearInterval(timer)
    timer = undefined
  }
  const onVisibility = () => (document.visibilityState === 'hidden' ? pause() : run())

  return {
    get: () => now,
    read: () => Date.now(),
    subscribe(listener) {
      if (listeners.size === 0) {
        now = Date.now()
        document.addEventListener('visibilitychange', onVisibility)
        if (document.visibilityState !== 'hidden') run()
      }
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
        if (listeners.size === 0) {
          pause()
          document.removeEventListener('visibilitychange', onVisibility)
        }
      }
    },
  }
}

const ClockContext = createContext<Clock | null>(null)

export function NowProvider({ fixed, children }: { fixed?: number; children: ReactNode }) {
  const clock = useMemo(() => (fixed !== undefined ? createFixedClock(fixed) : createLiveClock()), [fixed])
  return <ClockContext.Provider value={clock}>{children}</ClockContext.Provider>
}

function useClock(): Clock {
  const clock = useContext(ClockContext)
  if (!clock) throw new Error('The clock hooks must be used inside <NowProvider>')
  return clock
}

/**
 * The time right now as a function, for event handlers and effects. Reading it never re-renders anything.
 * Use `useNowSelector` for anything that is drawn on screen.
 */
export function useNowReader(): () => number {
  return useClock().read
}

export function shallowEqual<T>(a: T, b: T): boolean {
  if (Object.is(a, b)) return true
  if (typeof a !== 'object' || typeof b !== 'object' || a === null || b === null) return false
  if (Array.isArray(a) || Array.isArray(b)) {
    if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false
    return a.every((item, i) => Object.is(item, b[i]))
  }
  const keysA = Object.keys(a)
  if (keysA.length !== Object.keys(b).length) return false
  return keysA.every((key) => Object.prototype.hasOwnProperty.call(b, key) && Object.is((a as never)[key], (b as never)[key]))
}

/**
 * What the current time means to this component. `select` runs on every tick; the component re-renders
 * only when its result changes (`isEqual`, default `Object.is`; use `shallowEqual` for small objects and
 * arrays). `select` may close over props and state: it is read fresh on every render.
 */
export function useNowSelector<T>(select: (now: number) => T, isEqual: (a: T, b: T) => boolean = Object.is): T {
  const clock = useClock()
  const last = useRef<{ value: T } | null>(null)
  const getSnapshot = () => {
    const value = select(clock.get())
    if (last.current && isEqual(last.current.value, value)) return last.current.value
    last.current = { value }
    return value
  }
  return useSyncExternalStore(clock.subscribe, getSnapshot, getSnapshot)
}

/**
 * Start of the current local day. Pass it to `formatClock` in place of `now`: that function only uses
 * the day of `now`, so the label re-renders at midnight instead of every second.
 */
export function useToday(): number {
  return useNowSelector(startOfDay)
}

/** "2 min ago" since `since`, or '' when there is no such moment. Re-renders when the words change. */
export function useAge(since: number | undefined): string {
  return useNowSelector((now) => (since === undefined ? '' : formatAge(Math.max(0, now - since))))
}
