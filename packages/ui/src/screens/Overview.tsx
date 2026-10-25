import * as m from 'motion/react-m'
import { useMemo } from 'react'
import { BalanceTile, ProviderPanel } from '@/components/limit/ProviderPanel'
import { TideHero } from '@/components/limit/TideHero'
import { NoReadingsYet, PcStatusNote } from '@/components/phone/pc'
import { ErrorState, LoadingCards } from '@/components/ScreenStates'
import { useSnapshotSource, useSnapshots } from '@/data/SnapshotSource'
import { groupForOverview, nextReset } from '@/lib/snapshot-view'
import { useNowSelector } from '@/state/now'

/** Panels rise in one after another on first paint; reduced motion turns this into nothing. */
const rise = (i: number) => ({
  initial: { opacity: 0, y: 10 },
  animate: { opacity: 1, y: 0 },
  transition: { type: 'spring' as const, duration: 0.5, bounce: 0, delay: Math.min(i, 8) * 0.06 },
})

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
      {next && <TideHero snapshot={next} />}
      {groups.limits.length > 0 && (
        <section aria-label="Limits">
          <ul className="grid grid-cols-1 items-start gap-4 lg:grid-cols-2">
            {groups.limits.map((g, i) => (
              <m.li key={g.key} {...rise(i)}>
                <ProviderPanel provider={g.provider} account={g.account} windows={g.windows} />
              </m.li>
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
            {groups.balances.map((s, i) => (
              <m.li key={s.id} {...rise(groups.limits.length + i)}>
                <BalanceTile snapshot={s} />
              </m.li>
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
