import { BellOff, BellPlus, Trash2 } from 'lucide-react'
import { useMemo, type ReactNode } from 'react'
import type { Snapshot } from '@cooldown/snapshot-schema'
import { Countdown, ProviderMark } from '@/components/limit/atoms'
import { PermissionNudge } from '@/components/phone/permissions'
import { EmptyState, ErrorState, LoadingCards } from '@/components/ScreenStates'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { useSnapshotSource, useSnapshots } from '@/data/SnapshotSource'
import { providerInfo, windowTitle } from '@/lib/providers'
import { alertIdFor, canAlert, formatClock, parseTime, snapshotIdFromAlertId, sortSnapshots } from '@/lib/snapshot-view'
import { useAlerts, type AlertEntry } from '@/state/alerts'
import { shallowEqual, useNowSelector, useToday } from '@/state/now'
import { Link } from '@/state/router'

export function AlertsScreen() {
  const { alerts, setEnabled, remove, toggle } = useAlerts()
  const { data: snapshots, loading, error, reload } = useSnapshots()
  const source = useSnapshotSource()
  const today = useToday()
  const armedIds = useMemo(() => new Set(alerts.filter((a) => a.rangAt === undefined).map((a) => a.id)), [alerts])
  const sortedSnapshots = useMemo(() => (snapshots ? sortSnapshots(snapshots) : []), [snapshots])
  // The windows that can still take a bell. Changes only when a reset passes, not every second.
  const available = useNowSelector(
    (now) => sortedSnapshots.filter((s) => canAlert(s, now) && !armedIds.has(alertIdFor(s))),
    shallowEqual,
  )

  if (loading && !snapshots) return <LoadingCards count={2} />
  if (!snapshots) return <ErrorState title="Could not read your limits" detail={error?.message} onRetry={reload} />

  const onPhone = Boolean(source.phone)
  const sorted = [...alerts].sort((a, b) => Date.parse(a.fireAt) - Date.parse(b.fireAt))

  return (
    <div className="flex max-w-3xl flex-col gap-8">
      {onPhone && <PermissionNudge />}
      <p className="max-w-prose text-sm text-muted-foreground">
        {onPhone ? (
          <>
            <strong className="font-semibold text-foreground">Bells are kept on your PC.</strong> Setting or pausing one
            here sends the change to the PC, and this phone rings it at the reset time, even with the app closed. Resets
            can move, so the PC keeps the time up to date.
          </>
        ) : (
          <>
            A bell rings once when a limit window resets. Cooldown checks the reset time again shortly before ringing,
            because resets can move.
            {source.isSample && ' This is sample data, so bells set here are not scheduled with the system yet.'}
          </>
        )}
      </p>

      <section aria-labelledby="bells-heading" className="flex flex-col gap-3">
        <h2 id="bells-heading" className="text-base font-semibold">
          Your bells
        </h2>
        {sorted.length === 0 ? (
          <EmptyState
            icon={BellOff}
            title="No bells set"
            action={
              <Button asChild>
                <Link to={{ name: 'overview' }}>Choose a limit on the overview</Link>
              </Button>
            }
          >
            Open any limit that has a reset time and choose “Notify me when it resets”. It will appear here with the
            exact time it rings.
          </EmptyState>
        ) : (
          <ul className="divide-y rounded-xl border bg-card">
            {sorted.map((alert) => (
              <AlertRow
                key={alert.id}
                alert={alert}
                snapshot={snapshots.find((s) => alertIdFor(s) === alert.id)}
                today={today}
                onEnabled={(enabled) => setEnabled(alert.id, enabled)}
                onRemove={onPhone ? undefined : () => remove(alert.id)}
              />
            ))}
          </ul>
        )}
      </section>

      {available.length > 0 && (
        <section aria-labelledby="available-heading" className="flex flex-col gap-3">
          <h2 id="available-heading" className="text-base font-semibold">
            Set a bell
          </h2>
          <ul className="divide-y rounded-xl border bg-card">
            {available.map((s) => (
              <AvailableRow key={s.id} snapshot={s} today={today} onSet={() => toggle(s)} />
            ))}
          </ul>
        </section>
      )}
    </div>
  )
}

function Identity({ provider, title, children }: { provider: string; title: string; children: ReactNode }) {
  return (
    <div className="flex min-w-0 flex-1 items-center gap-3">
      <ProviderMark provider={provider} />
      <div className="min-w-0">
        <p className="text-base font-medium [overflow-wrap:anywhere]">
          {providerInfo(provider).name} <span className="font-normal text-muted-foreground">{title}</span>
        </p>
        <p className="num text-sm text-muted-foreground">{children}</p>
      </div>
    </div>
  )
}

function AlertRow({
  alert,
  snapshot,
  today,
  onEnabled,
  onRemove,
}: {
  alert: AlertEntry
  snapshot: Snapshot | undefined
  /** Start of today: `formatClock` only needs the day. */
  today: number
  onEnabled: (enabled: boolean) => void
  /** Omitted when the bell cannot be deleted here (on a phone the PC owns it: pause it instead). */
  onRemove?: () => void
}) {
  const fireAt = Date.parse(alert.fireAt)
  const rang = alert.rangAt !== undefined
  const fallbackId = snapshotIdFromAlertId(alert.id) ?? alert.id
  const provider = snapshot?.provider ?? fallbackId.split(':')[0] ?? 'unknown'
  const title = snapshot ? windowTitle(snapshot.provider, snapshot.window) : fallbackId.split(':').slice(1).join(' ')
  const label = `${providerInfo(provider).name} ${title}`

  return (
    <li className="flex items-center gap-2 p-3 sm:gap-3 sm:p-4">
      <Identity provider={provider} title={title}>
        {rang ? (
          <>Rang at {formatClock(fireAt, today)}</>
        ) : !alert.enabled ? (
          <>Paused, would ring {formatClock(fireAt, today)}</>
        ) : (
          <>
            Rings {formatClock(fireAt, today)}, in <Countdown target={fireAt} />
          </>
        )}
      </Identity>
      <label className="flex size-11 shrink-0 cursor-pointer items-center justify-center">
        <Switch
          checked={alert.enabled && !rang}
          onCheckedChange={onEnabled}
          aria-label={`Bell for ${label}`}
        />
      </label>
      {onRemove && (
        <Button variant="ghost" size="icon" onClick={onRemove} aria-label={`Delete bell for ${label}`}>
          <Trash2 aria-hidden="true" />
        </Button>
      )}
    </li>
  )
}

function AvailableRow({ snapshot, today, onSet }: { snapshot: Snapshot; today: number; onSet: () => void }) {
  const at = parseTime(snapshot.resetsAt)
  const title = windowTitle(snapshot.provider, snapshot.window)
  return (
    <li className="flex items-center gap-3 p-3 sm:p-4">
      <Identity provider={snapshot.provider} title={title}>
        {at !== undefined && <>Resets {formatClock(at, today)}</>}
      </Identity>
      <Button variant="outline" onClick={onSet} aria-label={`Set bell for ${providerInfo(snapshot.provider).name} ${title}`}>
        <BellPlus aria-hidden="true" />
        <span className="hidden sm:inline">Set bell</span>
      </Button>
    </li>
  )
}
