import { useMemo } from 'react'
import type { Snapshot } from '@cooldown/snapshot-schema'
import { Countdown } from '@/components/limit/atoms'
import { BellButton } from '@/components/limit/BellButton'
import { BalanceTile, ProviderPanel } from '@/components/limit/ProviderPanel'
import { NoReadingsYet, PcStatusNote } from '@/components/phone/pc'
import { ErrorState, LoadingCards } from '@/components/ScreenStates'
import { useSnapshotSource, useSnapshots } from '@/data/SnapshotSource'
import { providerInfo, windowTitle } from '@/lib/providers'
import { formatClock, groupForOverview, nextReset, parseTime, percentUsed } from '@/lib/snapshot-view'
import { useNowSelector, useToday } from '@/state/now'

export function OverviewScreen() {
  const { data, loading, error, reload } = useSnapshots()
  const onPhone = Boolean(useSnapshotSource().phone)
  const groups = useMemo(() => groupForOverview(data ?? []), [data])
  const snapshots = useMemo(() => groups.limits.flatMap((g) => g.windows), [groups])
  // Changes only when a reset passes, so the page does not render again every second.
  const next = useNowSelector((now) => nextReset(snapshots, now))

  if (loading && !data) return <LoadingCards />
  if (!data) {
    return (
      <ErrorState
        title="Could not read your limits"
        detail={error?.message ?? 'The data source did not answer. Nothing has been lost; try again.'}
        onRetry={reload}
      />
    )
  }

  if (onPhone && data.length === 0) return <NoReadingsYet />

  return (
    <div className="flex flex-col gap-6">
      {onPhone && <PcStatusNote />}
      {next && <NextBell snapshot={next} />}
      {groups.limits.length > 0 && (
        <section aria-label="Limits">
          <ul className="grid grid-cols-1 items-start gap-4 lg:grid-cols-2">
            {groups.limits.map((g) => (
              <li key={g.key}>
                <ProviderPanel provider={g.provider} account={g.account} windows={g.windows} />
              </li>
            ))}
          </ul>
        </section>
      )}
      {groups.balances.length > 0 && (
        <section aria-labelledby="balances-heading" className="flex flex-col gap-3">
          <h2 id="balances-heading" className="text-base font-semibold">
            Balances
          </h2>
          <ul className="grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3">
            {groups.balances.map((s) => (
              <li key={s.id}>
                <BalanceTile snapshot={s} />
              </li>
            ))}
          </ul>
        </section>
      )}
      {error && (
        <p role="status" className="text-sm text-sand-ink">
          The last refresh failed. Values above are the last good ones.
        </p>
      )}
    </div>
  )
}

/** The soonest reset across all limits, with its own bell. The one place the page leads with time. */
function NextBell({ snapshot }: { snapshot: Snapshot }) {
  const today = useToday()
  const resetsAt = parseTime(snapshot.resetsAt)
  if (resetsAt === undefined) return null
  const name = `${providerInfo(snapshot.provider).name} ${windowTitle(snapshot.provider, snapshot.window).toLowerCase()}`
  const percent = percentUsed(snapshot)
  // "Back" only fits a limit that is used up; anything else simply resets.
  const verb = percent !== undefined && percent >= 100 ? 'is back in' : 'resets in'
  return (
    <section
      aria-label="Next reset"
      className="flex flex-col gap-4 rounded-xl border border-tide/30 bg-tide-soft p-4 sm:flex-row sm:items-center sm:justify-between sm:p-5"
    >
      <div className="min-w-0">
        <h2 className="text-xl leading-snug font-semibold text-foreground">
          {name} {verb} <Countdown target={resetsAt} className="text-2xl" />
        </h2>
        <p className="num mt-1 text-sm text-muted-foreground">
          Next reset, at {formatClock(resetsAt, today)}
          {percent !== undefined && percent >= 100 ? '. The limit is reached now.' : percent !== undefined ? `. ${Math.round(percent)}% used.` : '.'}
        </p>
      </div>
      <div className="sm:w-72 sm:shrink-0">
        <BellButton snapshot={snapshot} />
      </div>
    </section>
  )
}
