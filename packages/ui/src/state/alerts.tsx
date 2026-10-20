/**
 * Alert state for the whole UI.
 *
 * Two modes, picked by the data source:
 *  - The source has `alerts` (the desktop backend): alerts are stored there and the backend's own
 *    timer rings them, even with the window closed. This provider mirrors that list and writes
 *    through to it.
 *  - Otherwise (sample data, browser preview): alerts live here in memory and a toast rings them.
 */
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { toast } from 'sonner'
import type { AlertIntent, Snapshot } from '@cooldown/snapshot-schema'
import { useSnapshotSource, useSnapshots } from '@/data/SnapshotSource'
import { shareStructure } from '@/data/share-structure'
import { providerInfo, windowTitle } from '@/lib/providers'
import { alertIdFor, canAlert, formatClock, makeAlertIntent, parseTime } from '@/lib/snapshot-view'
import { shallowEqual, useNowReader, useNowSelector } from './now'

/** An AlertIntent plus the UI-only fact that it has already rung. */
export type AlertEntry = AlertIntent & { rangAt?: number }

type AlertsContextValue = {
  alerts: AlertEntry[]
  /** True when a bell is armed for this snapshot's reset. */
  isArmed: (snapshot: Snapshot) => boolean
  /** Arms a bell for the snapshot, or removes it when already armed. */
  toggle: (snapshot: Snapshot) => void
  setEnabled: (alertId: string, enabled: boolean) => void
  remove: (alertId: string) => void
}

const AlertsContext = createContext<AlertsContextValue | null>(null)

function stripRang({ id, fireAt, enabled }: AlertEntry): AlertIntent {
  return { id, fireAt, enabled }
}

/**
 * The backend's list, plus what the UI already knew had rung. An enabled alert that vanished from
 * the backend after its time was rung (the backend removes an alert when it fires).
 */
export function mergeBackendAlerts(prev: AlertEntry[], list: AlertIntent[], now: number): AlertEntry[] {
  const next: AlertEntry[] = list.map((a) => ({ ...a }))
  for (const a of prev) {
    if (list.some((l) => l.id === a.id)) continue
    if (a.rangAt !== undefined) next.push(a)
    else if (a.enabled && Date.parse(a.fireAt) <= now) next.push({ ...a, rangAt: now })
  }
  return next
}

const NO_IDS: string[] = []

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err)
}

export function AlertsProvider({
  children,
  initialArmed = [],
}: {
  children: ReactNode
  /** Snapshot ids to arm once their snapshots have loaded (in-memory mode only). */
  initialArmed?: string[]
}) {
  const source = useSnapshotSource()
  const backend = source.alerts
  const [alerts, setAlerts] = useState<AlertEntry[]>([])
  const alertsRef = useRef(alerts)
  alertsRef.current = alerts
  const { data: snapshots } = useSnapshots()
  // Time for event handlers and effects. Reading it does not subscribe: this provider renders when its
  // alerts change, not every second.
  const readNow = useNowReader()
  const seeded = useRef(false)

  const labelFor = useCallback(
    (alertId: string) => {
      const snapshot = snapshots?.find((s) => alertIdFor(s) === alertId)
      return snapshot ? `${providerInfo(snapshot.provider).name} ${windowTitle(snapshot.provider, snapshot.window)}` : 'A limit'
    },
    [snapshots],
  )
  const labelRef = useRef(labelFor)
  labelRef.current = labelFor

  // Backend mode: load the list now and whenever the source reports a change.
  useEffect(() => {
    if (!backend) return
    let cancelled = false
    const load = () => {
      backend.list().then(
        (list) => {
          if (cancelled) return
          const next = mergeBackendAlerts(alertsRef.current, list, readNow())
          for (const a of next) {
            const before = alertsRef.current.find((p) => p.id === a.id)
            if (a.rangAt !== undefined && before && before.rangAt === undefined) {
              toast.success(`${labelRef.current(a.id)} is back`, {
                description: `Reset at ${formatClock(Date.parse(a.fireAt), readNow())}.`,
              })
            }
          }
          // Same bells as before (the usual case after a poll): keep the same list so nothing re-renders.
          const shared = shareStructure(alertsRef.current, next)
          if (shared === alertsRef.current) return
          alertsRef.current = shared
          setAlerts(shared)
        },
        () => {
          // Keep showing what we had; the next change event retries.
        },
      )
    }
    load()
    const unsubscribe = source.subscribe(load)
    return () => {
      cancelled = true
      unsubscribe()
    }
  }, [source, backend, readNow])

  const persist = useCallback(
    (work: Promise<void> | undefined, failure: string) => {
      work?.catch((err: unknown) => {
        toast.error(failure, { description: errorText(err) })
        backend?.list().then((list) => setAlerts((prev) => mergeBackendAlerts(prev, list, readNow())), () => {})
      })
    },
    [backend, readNow],
  )

  // In-memory mode: seed once, when data first arrives, so the Alerts screen shows a real example.
  useEffect(() => {
    if (backend || seeded.current || !snapshots) return
    seeded.current = true
    const seeds: AlertEntry[] = []
    for (const id of initialArmed) {
      const snapshot = snapshots.find((s) => s.id === id)
      const intent = snapshot ? makeAlertIntent(snapshot) : undefined
      if (intent) seeds.push(intent)
    }
    if (seeds.length) setAlerts(seeds)
  }, [backend, snapshots, initialArmed])

  // In-memory mode: an armed alert follows the latest known resetsAt (replace, never stack).
  useEffect(() => {
    if (backend || !snapshots) return
    setAlerts((prev) => {
      let changed = false
      const next = prev.map((a) => {
        if (a.rangAt !== undefined) return a
        const snapshot = snapshots.find((s) => alertIdFor(s) === a.id)
        const at = parseTime(snapshot?.resetsAt)
        if (snapshot && at !== undefined && at > readNow() && snapshot.resetsAt && snapshot.resetsAt !== a.fireAt) {
          changed = true
          return { ...a, fireAt: snapshot.resetsAt }
        }
        return a
      })
      return changed ? next : prev
    })
  }, [backend, snapshots])

  // In-memory mode: when an armed alert's time passes, show a toast once. The selector answers "which
  // alerts are due"; it changes only when one rings, so nothing here runs every second.
  const dueIds = useNowSelector(
    (now) =>
      backend
        ? NO_IDS
        : alerts.filter((a) => a.enabled && a.rangAt === undefined && Date.parse(a.fireAt) <= now).map((a) => a.id),
    shallowEqual,
  )
  useEffect(() => {
    if (dueIds.length === 0) return
    const at = readNow()
    const due = alertsRef.current.filter((a) => dueIds.includes(a.id) && a.rangAt === undefined)
    if (due.length === 0) return
    setAlerts((prev) => prev.map((a) => (dueIds.includes(a.id) && a.rangAt === undefined ? { ...a, rangAt: at } : a)))
    for (const a of due) {
      toast.success(`${labelRef.current(a.id)} is back`, { description: `Reset at ${formatClock(Date.parse(a.fireAt), at)}.` })
    }
  }, [dueIds, readNow])

  const isArmed = useCallback(
    (snapshot: Snapshot) => {
      const id = alertIdFor(snapshot)
      return alerts.some((a) => a.id === id && a.enabled && a.rangAt === undefined)
    },
    [alerts],
  )

  const toggle = useCallback(
    (snapshot: Snapshot) => {
      const id = alertIdFor(snapshot)
      const existing = alerts.find((a) => a.id === id)
      const label = `${providerInfo(snapshot.provider).name} ${windowTitle(snapshot.provider, snapshot.window)}`
      if (existing && existing.enabled && existing.rangAt === undefined) {
        setAlerts((prev) => prev.filter((a) => a.id !== id))
        persist(backend?.remove(id), 'Could not remove the bell')
        toast(`Bell removed for ${label}`)
        return
      }
      if (!canAlert(snapshot, readNow())) return
      const intent = makeAlertIntent(snapshot)
      if (!intent) return
      setAlerts((prev) => [...prev.filter((a) => a.id !== id), intent])
      persist(backend?.set(intent), 'Could not set the bell')
      toast.success(`Bell set for ${label}`, {
        description: `Rings at ${formatClock(Date.parse(intent.fireAt), readNow())}.`,
      })
    },
    [alerts, backend, persist],
  )

  const setEnabled = useCallback(
    (alertId: string, enabled: boolean) => {
      const current = alerts.find((a) => a.id === alertId)
      if (!current) return
      let next: AlertEntry = current
      if (!enabled) {
        next = { ...current, enabled: false }
      } else if (current.rangAt !== undefined) {
        // Turning an alert that already rang back on re-arms it, but only if the window has a future reset.
        const snapshot = snapshots?.find((s) => alertIdFor(s) === alertId)
        const intent = snapshot && canAlert(snapshot, readNow()) ? makeAlertIntent(snapshot) : undefined
        if (!intent) return
        next = { ...intent }
      } else {
        next = { ...current, enabled: true }
      }
      setAlerts((prev) => prev.map((a) => (a.id === alertId ? next : a)))
      persist(backend?.set(stripRang(next)), 'Could not change the bell')
    },
    [alerts, snapshots, backend, persist],
  )

  const remove = useCallback(
    (alertId: string) => {
      const removed = alerts.find((a) => a.id === alertId)
      setAlerts((prev) => prev.filter((a) => a.id !== alertId))
      persist(backend?.remove(alertId), 'Could not delete the bell')
      if (removed) {
        toast('Bell deleted', {
          action: {
            label: 'Undo',
            onClick: () => {
              setAlerts((prev) => (prev.some((a) => a.id === removed.id) ? prev : [...prev, removed]))
              if (removed.rangAt === undefined) persist(backend?.set(stripRang(removed)), 'Could not restore the bell')
            },
          },
        })
      }
    },
    [alerts, backend, persist],
  )

  const value = useMemo(
    () => ({ alerts, isArmed, toggle, setEnabled, remove }),
    [alerts, isArmed, toggle, setEnabled, remove],
  )
  return <AlertsContext.Provider value={value}>{children}</AlertsContext.Provider>
}

export function useAlerts(): AlertsContextValue {
  const ctx = useContext(AlertsContext)
  if (!ctx) throw new Error('useAlerts must be used inside <AlertsProvider>')
  return ctx
}
