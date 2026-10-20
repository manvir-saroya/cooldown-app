import { Info } from 'lucide-react'
import { lazy, Suspense, type ReactNode } from 'react'
import type { Snapshot, WindowKind } from '@cooldown/snapshot-schema'
import { AgeLabel, Countdown, SourceBadge, StatusBadge } from '@/components/limit/atoms'
import { LimitCard } from '@/components/limit/LimitCard'
import { EmptyState, ErrorState, LoadingCards } from '@/components/ScreenStates'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useSnapshotSource, useSnapshots, useSourceQuery } from '@/data/SnapshotSource'
import type { HistorySeries } from '@/data/types'
import { providerInfo, SOURCE_COPY, STATUS_COPY, windowTitle } from '@/lib/providers'
import {
  effectiveStatus,
  formatClock,
  formatFull,
  historyTitle,
  parseTime,
  snapshotsForProvider,
  windowLabel,
  windowLengthMs,
} from '@/lib/snapshot-view'
import { Link, navigate } from '@/state/router'
import { shallowEqual, useNowSelector, useToday } from '@/state/now'

// The chart is the only user of Recharts (about 350 KB). It loads here, on the screen that draws it,
// and not at all on a phone, where there is no history to draw.
const HistoryChart = lazy(() => import('@/components/HistoryChart').then((m) => ({ default: m.HistoryChart })))

export function ProviderScreen({ provider, window }: { provider: string; window?: WindowKind }) {
  const { data, loading, error, reload } = useSnapshots()

  if (loading && !data) return <LoadingCards count={1} />
  if (!data) return <ErrorState title="Could not read this provider" detail={error?.message} onRetry={reload} />

  const snapshots = snapshotsForProvider(data, provider)
  const first = snapshots[0]
  if (!first) {
    return (
      <EmptyState
        icon={Info}
        title="No readings for this provider"
        action={
          <Button asChild variant="outline">
            <Link to={{ name: 'overview' }}>Back to overview</Link>
          </Button>
        }
      >
        Cooldown has not seen a limit or balance for “{provider}” yet. Settings lists the sources it reads and what each one needs.
      </EmptyState>
    )
  }
  const active = snapshots.find((s) => s.window === window) ?? first

  const detail = <WindowDetail key={active.id} snapshot={active} />

  // With several windows the detail is the tab panel, so each tab points at a panel that exists.
  if (snapshots.length === 1) return <div className="flex flex-col gap-6">{detail}</div>
  return (
    <Tabs
      value={active.window}
      onValueChange={(w) => navigate({ name: 'provider', provider, window: w as WindowKind }, { replace: true })}
      className="gap-6"
    >
      <TabsList aria-label={`${providerInfo(provider).name} windows`} className="h-auto! w-full justify-start sm:w-fit">
        {snapshots.map((s) => (
          <TabsTrigger key={s.id} value={s.window} className="min-h-11 flex-1 sm:flex-none lg:min-h-9">
            {windowTitle(s.provider, s.window)}
          </TabsTrigger>
        ))}
      </TabsList>
      <TabsContent value={active.window} className="flex flex-col gap-6">
        {detail}
      </TabsContent>
    </Tabs>
  )
}

function WindowDetail({ snapshot }: { snapshot: Snapshot }) {
  const provider = providerInfo(snapshot.provider)
  const onPhone = Boolean(useSnapshotSource().phone)
  return (
    <>
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-[minmax(0,24rem)_minmax(0,1fr)] lg:items-start">
        <LimitCard snapshot={snapshot} linked={false} />
        {onPhone ? (
          <NoHistoryOnPhone />
        ) : (
          <Suspense fallback={<HistoryLoading snapshot={snapshot} />}>
            <HistoryChart snapshot={snapshot} />
          </Suspense>
        )}
      </div>
      <div className="grid grid-cols-1 gap-4 lg:grid-cols-2">
        <ResetSchedule snapshot={snapshot} />
        <SourceDetails snapshot={snapshot} />
      </div>
      <section aria-labelledby="how-heading" className="max-w-prose">
        <h2 id="how-heading" className="text-base font-semibold">
          How this number is obtained
        </h2>
        <p className="mt-1.5 text-sm leading-relaxed text-muted-foreground">{provider.howObtained}</p>
      </section>
    </>
  )
}

/** The phone keeps no history: it only holds the latest sync. Say where the chart lives instead of drawing one dot. */
function NoHistoryOnPhone() {
  return (
    <Plate id="history-phone-heading" title="History">
      <p className="text-sm text-muted-foreground">
        History is kept on your PC. This phone only holds the latest reading, so there is no chart here. Open Cooldown
        on your PC to see how this limit has moved.
      </p>
    </Plate>
  )
}

/** Same frame and height as the chart, so the page does not jump when the chart arrives. */
function HistoryLoading({ snapshot }: { snapshot: Snapshot }) {
  return (
    <section aria-busy="true" aria-labelledby="history-loading" className="flex min-w-0 flex-col gap-3 rounded-xl border bg-card p-4 sm:p-5">
      <h2 id="history-loading" className="text-base font-semibold">
        {historyTitle(snapshot)}
      </h2>
      <Skeleton className="h-56 w-full" />
    </section>
  )
}

function Plate({ id, title, children }: { id: string; title: string; children: ReactNode }) {
  return (
    <section aria-labelledby={id} className="rounded-xl border bg-card p-4 sm:p-5">
      <h2 id={id} className="text-base font-semibold">
        {title}
      </h2>
      <div className="mt-3">{children}</div>
    </section>
  )
}

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid grid-cols-1 gap-0.5 border-t py-3 first:border-t-0 first:pt-0 last:pb-0 sm:grid-cols-[9rem_minmax(0,1fr)] sm:gap-4">
      <dt className="text-sm text-muted-foreground">{label}</dt>
      <dd className="min-w-0 text-sm">{children}</dd>
    </div>
  )
}

function ResetSchedule({ snapshot }: { snapshot: Snapshot }) {
  const today = useToday()
  const resetsAt = parseTime(snapshot.resetsAt)
  const resetPassed = useNowSelector((now) => effectiveStatus(snapshot, now).resetPassed)
  const length = windowLengthMs(snapshot.window)
  const onPhone = Boolean(useSnapshotSource().phone)
  const history = useSourceQuery<HistorySeries | undefined>(
    (s) => (s.phone ? Promise.resolve(undefined) : s.getHistory(snapshot.id, '30d')),
    [snapshot.id],
  )
  const resets = history.data?.resets
  const past = useNowSelector((now) => (resets ?? []).filter((t) => t <= now).slice(-3).reverse(), shallowEqual)

  return (
    <Plate id="schedule-heading" title="Reset schedule">
      {snapshot.window === 'balance' ? (
        <p className="text-sm text-muted-foreground">
          A balance does not reset. It only changes when you spend or top up, so there is no bell to set for it.
        </p>
      ) : resetsAt === undefined ? (
        <p className="text-sm text-muted-foreground">
          No reading has said when this window resets yet. The next reset time shows here once one does.
        </p>
      ) : (
        <dl>
          <Row label="Next reset">
            {resetPassed ? (
              <span className="text-sand-ink">The reset time has passed ({formatClock(resetsAt, today)}). Waiting for a fresh reading.</span>
            ) : (
              <>
                <span className="num font-medium">{formatFull(resetsAt)}</span>
                <span className="text-muted-foreground">
                  {' '}
                  in <Countdown target={resetsAt} />
                </span>
              </>
            )}
          </Row>
          {length !== undefined && <Row label="Window length">{windowLabel(snapshot.window)}, {formatLength(length)}</Row>}
          {!onPhone && (
            <Row label="Earlier resets">
              {history.data === undefined ? (
                <span className="text-muted-foreground">{history.error ? 'History could not be loaded.' : 'Loading…'}</span>
              ) : past.length === 0 ? (
                <span className="text-muted-foreground">None seen in the last 30 days.</span>
              ) : (
                <ul className="num space-y-1">
                  {past.map((t) => (
                    <li key={t}>{formatClock(t, today)}</li>
                  ))}
                </ul>
              )}
            </Row>
          )}
        </dl>
      )}
    </Plate>
  )
}

function formatLength(ms: number): string {
  const hours = ms / 3_600_000
  return hours >= 48 ? `${Math.round(hours / 24)} days` : `${hours} hours`
}

function SourceDetails({ snapshot }: { snapshot: Snapshot }) {
  const { status, resetPassed } = useNowSelector((now) => effectiveStatus(snapshot, now), shallowEqual)
  const provider = providerInfo(snapshot.provider)
  const observed = parseTime(snapshot.observedAt)
  return (
    <Plate id="source-heading" title="Source details">
      <dl>
        <Row label="Source">
          <div className="flex flex-wrap items-center gap-2">
            <SourceBadge kind={snapshot.source.kind} />
            <span className="text-muted-foreground">{SOURCE_COPY[snapshot.source.kind].meaning}</span>
          </div>
        </Row>
        <Row label="Adapter">
          <span className="num">
            {snapshot.source.adapter} <span className="text-muted-foreground">v{snapshot.source.adapterVersion}</span>
          </span>
        </Row>
        <Row label="Last observed">
          <div className="flex flex-col gap-0.5">
            {observed !== undefined && <span className="num">{formatFull(observed)}</span>}
            <AgeLabel observedAt={snapshot.observedAt} status={status} prefix={false} />
          </div>
        </Row>
        <Row label="Status">
          <div className="flex flex-col items-start gap-1.5">
            <StatusBadge status={status} />
            <span className="text-muted-foreground">
              {resetPassed && status === 'stale'
                ? 'The window this reading describes has ended. Treated as stale until a new reading arrives.'
                : (snapshot.note ?? STATUS_COPY[status].meaning)}
            </span>
          </div>
        </Row>
        <Row label="Updates">{provider.freshness}</Row>
      </dl>
    </Plate>
  )
}
