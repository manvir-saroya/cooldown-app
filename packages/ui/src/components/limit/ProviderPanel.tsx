/**
 * Overview panels. One panel per provider account answers "can I use it now?" in its header, then
 * lists that provider's limit windows as compact rows: meter, figure, reset and a bell. Where the
 * numbers come from (source, status, age) is said once per panel, and any problem is said once, in
 * words. Balances get their own small tiles. The full card with every detail stays on the provider
 * screen (LimitCard).
 */
import { Bell, BellRing, CircleCheck, CircleOff, Clock, Hourglass, TriangleAlert } from 'lucide-react'
import * as m from 'motion/react-m'
import { memo } from 'react'
import type { Snapshot, Status } from '@cooldown/snapshot-schema'
import { Button } from '@/components/ui/button'
import { providerInfo, windowTitle } from '@/lib/providers'
import {
  canAlert,
  effectiveStatus,
  formatClock,
  formatMoney,
  meterTone,
  parseTime,
  percentUsed,
  providerVerdict,
  SEVERITY,
  windowLabel,
  type Verdict,
} from '@/lib/snapshot-view'
import { cn } from '@/lib/utils'
import { useAlerts } from '@/state/alerts'
import { Link } from '@/state/router'
import { shallowEqual, useNowSelector, useToday } from '@/state/now'
import { AgeLabel, Countdown, ProviderMark, SourceBadge, StatusBadge } from './atoms'
import { TideStaff, type Certainty } from './TideStaff'

const STATUS_NOTE: Record<Exclude<Status, 'ok'>, string> = {
  stale: 'Not refreshed lately. The last good figures are shown.',
  degraded: 'The source is struggling, so these figures may be off.',
  unavailable: 'Cannot be read right now. The last good figures are shown.',
}

/** The worst status across the windows, and the distinct problem sentences to show once. */
function panelHealth(windows: Snapshot[], now: number) {
  let worst: Status = 'ok'
  // Each distinct problem once, with the windows it applies to.
  const byNote = new Map<string, string[]>()
  let oldest = windows[0]!
  for (const s of windows) {
    const { status, resetPassed } = effectiveStatus(s, now)
    if (SEVERITY[status] > SEVERITY[worst]) worst = status
    if (Date.parse(s.observedAt) < Date.parse(oldest.observedAt)) oldest = s
    if (status === 'ok' || (resetPassed && status === 'stale')) continue
    const note = s.note ?? STATUS_NOTE[status]
    byNote.set(note, [...(byNote.get(note) ?? []), windowLabel(s.window)])
  }
  // A problem every window shares needs no window name in front of it.
  const notes = [...byNote].map(([note, labels]) =>
    labels.length === windows.length && windows.length > 1 ? note : `${labels.join(', ')}: ${note}`,
  )
  return { worst, notes, oldest }
}

export const ProviderPanel = memo(function ProviderPanel({
  provider,
  account,
  windows,
}: {
  provider: string
  account: string
  windows: Snapshot[]
}) {
  const info = providerInfo(provider)
  const verdict = useNowSelector((now) => providerVerdict(windows, now), shallowEqual)
  const health = useNowSelector((now) => panelHealth(windows, now), healthEqual)
  const headingId = `panel-${provider}-${account}`.replace(/[^a-z0-9-]/gi, '-')
  // Every window of a provider comes from one adapter today; say the weakest source if they differ.
  const source = windows.reduce((a, s) => (a.source.kind === 'official' ? s : a), windows[0]!).source

  return (
    <article
      aria-labelledby={headingId}
      data-verdict={verdict.kind}
      className={cn(
        'flex flex-col rounded-xl border bg-card text-card-foreground',
        verdict.kind === 'blocked' && 'border-shoal/50',
      )}
    >
      <header className="flex items-center gap-3 px-4 pt-4 sm:px-5">
        <ProviderMark provider={provider} />
        <div className="min-w-0 flex-1">
          <h2 id={headingId} className="truncate text-base leading-tight font-semibold">
            <Link
              to={{ name: 'provider', provider, window: windows[0]?.window }}
              className="rounded-sm outline-none hover:underline hover:underline-offset-4 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
            >
              {info.name}
            </Link>
          </h2>
          <p className="truncate text-sm text-muted-foreground">{account}</p>
        </div>
        <VerdictLine verdict={verdict} />
      </header>

      {health.notes.length > 0 && (
        <div
          className={cn(
            'mx-4 mt-3 flex items-start gap-2 rounded-md px-3 py-2 text-sm sm:mx-5',
            health.worst === 'unavailable' ? 'bg-muted text-muted-foreground' : 'bg-sand-soft text-sand-ink',
          )}
        >
          {health.worst === 'unavailable' ? (
            <CircleOff aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
          ) : health.worst === 'degraded' ? (
            <TriangleAlert aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
          ) : (
            <Clock aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
          )}
          <ul className="space-y-0.5">
            {health.notes.map((n) => (
              <li key={n}>{n}</li>
            ))}
          </ul>
        </div>
      )}

      <ul className="mt-2 divide-y divide-border/70">
        {windows.map((s) => (
          <WindowRow key={s.id} snapshot={s} providerName={info.name} />
        ))}
      </ul>

      <footer className="mt-auto flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-border/70 px-4 py-2.5 sm:px-5">
        <SourceBadge kind={source.kind} />
        <StatusBadge status={health.worst} />
        <AgeLabel observedAt={health.oldest.observedAt} status={health.worst} className="ml-auto" />
      </footer>
    </article>
  )
})

function healthEqual(a: ReturnType<typeof panelHealth>, b: ReturnType<typeof panelHealth>) {
  return a.worst === b.worst && a.oldest === b.oldest && a.notes.join('\n') === b.notes.join('\n')
}

/** The header's answer. Words and an icon carry it; colour only backs them up. */
function VerdictLine({ verdict }: { verdict: Verdict }) {
  switch (verdict.kind) {
    case 'blocked':
      return (
        <p className="flex shrink-0 flex-col items-end text-right">
          <span className="flex items-center gap-1 text-xs font-medium text-shoal-ink">
            <Hourglass aria-hidden="true" className="size-3.5" />
            Limit reached
          </span>
          {verdict.until !== undefined ? (
            <span className="text-sm font-semibold text-shoal-ink">
              back in <Countdown target={verdict.until} />
            </span>
          ) : (
            <span className="text-sm text-muted-foreground">reset time not known</span>
          )}
        </p>
      )
    case 'low':
      return (
        <p className="flex shrink-0 items-center gap-1.5 rounded-md bg-sand-soft px-2 py-1 text-sm font-medium text-sand-ink">
          <TriangleAlert aria-hidden="true" className="size-4" />
          <span>
            {windowLabel(verdict.window)} at <span className="num">{Math.round(verdict.percent)}%</span>
          </span>
        </p>
      )
    case 'ready':
      return (
        <p className="flex shrink-0 items-center gap-1.5 text-sm font-semibold text-tide-ink">
          <CircleCheck aria-hidden="true" className="size-4" />
          Ready
        </p>
      )
    case 'unknown':
      return <p className="shrink-0 text-sm text-muted-foreground">No reading yet</p>
  }
}

/** One limit window: label and reset on the left, meter in the middle, figure and bell on the right. */
function WindowRow({ snapshot, providerName }: { snapshot: Snapshot; providerName: string }) {
  const { status, resetPassed } = useNowSelector((now) => effectiveStatus(snapshot, now), shallowEqual)
  const today = useToday()
  const percent = percentUsed(snapshot)
  const tone = meterTone(percent)
  const resetsAt = parseTime(snapshot.resetsAt)
  const title = windowTitle(snapshot.provider, snapshot.window)
  const full = percent !== undefined && percent >= 100 && !resetPassed
  // A bell whose time has passed (a phone may not have heard from the PC since) says it rang.
  const { alerts } = useAlerts()
  const bell = alerts.find((a) => a.enabled && a.id === `reset:${snapshot.provider}:${snapshot.account}:${snapshot.window}`)
  const bellAt = parseTime(bell?.fireAt)
  const rang = useNowSelector((now) => bellAt !== undefined && bellAt <= now)
  const certainty: Certainty =
    status === 'unavailable' ? 'unavailable' : status !== 'ok' || snapshot.source.kind === 'estimated' ? 'approximate' : 'measured'

  return (
    <li className="grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-x-3 gap-y-2 px-4 py-3 [grid-template-areas:'label_figure_bell'_'meter_meter_meter'] sm:grid-cols-[7.5rem_minmax(0,1fr)_4rem_auto] sm:px-5 sm:[grid-template-areas:'label_meter_figure_bell']">
      <div className="min-w-0 [grid-area:label]">
        <Link
          to={{ name: 'provider', provider: snapshot.provider, window: snapshot.window }}
          className="rounded-sm text-sm font-medium outline-none hover:underline hover:underline-offset-4 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
        >
          {title}
        </Link>
        <p className="num text-xs text-muted-foreground">
          {rang && bellAt !== undefined ? (
            <span className="text-tide-ink">Bell rang at {formatClock(bellAt, today)}</span>
          ) : resetsAt === undefined ? (
            percent === undefined ? 'no reading yet' : 'reset time not known yet'
          ) : resetPassed ? (
            <span className="text-sand-ink">reset at {formatClock(resetsAt, today)}, waiting for a reading</span>
          ) : (
            <>
              {full ? 'back in ' : 'resets in '}
              <Countdown target={resetsAt} className={cn('font-medium', full ? 'text-shoal-ink' : 'text-foreground')} />
            </>
          )}
        </p>
      </div>

      <div className="[grid-area:meter]">
        {percent !== undefined ? (
          <TideStaff
            percent={percent}
            tone={tone}
            certainty={certainty}
            label={`${providerName} ${title}, used`}
            showScale={false}
            className="[&_[data-slot=progress]]:h-2.5"
          />
        ) : (
          <div aria-hidden="true" className="h-2.5 rounded-sm border border-dashed border-border" />
        )}
      </div>

      <p className="num text-right [grid-area:figure]">
        {percent === undefined ? (
          <span className="text-lg font-semibold text-muted-foreground">—</span>
        ) : (
          <>
            <span className={cn('text-xl leading-none font-semibold', full && 'text-shoal-ink')}>{Math.round(percent)}</span>
            <span className="text-sm text-muted-foreground">%</span>
          </>
        )}
      </p>

      <div className="[grid-area:bell]">
        <BellToggle snapshot={snapshot} label={`${providerName} ${title.toLowerCase()}`} />
      </div>
    </li>
  )
}

/** The compact bell for a row. Its name says which limit, so a screen reader can tell rows apart. */
export function BellToggle({ snapshot, label }: { snapshot: Snapshot; label: string }) {
  const { isArmed, toggle, alerts } = useAlerts()
  const today = useToday()
  const canSetNow = useNowSelector((now) => canAlert(snapshot, now))
  const armed = isArmed(snapshot)
  const intent = alerts.find((a) => a.id === `reset:${snapshot.provider}:${snapshot.account}:${snapshot.window}`)
  const fireAt = parseTime(intent?.fireAt)
  const eligible = armed || canSetNow
  const name = armed ? `Bell set for when ${label} resets. Remove it` : `Notify me when ${label} resets`
  const hint = armed && fireAt !== undefined ? `Rings at ${formatClock(fireAt, today)}` : !eligible ? 'No future reset to ring at yet' : name

  return (
    <Button
      type="button"
      size="icon"
      variant={armed ? 'default' : 'ghost'}
      aria-pressed={armed}
      aria-label={name}
      title={hint}
      disabled={!eligible}
      onClick={() => toggle(snapshot)}
      className={cn('size-11 lg:size-9', !armed && 'text-muted-foreground hover:text-foreground')}
    >
      <m.span
        initial={false}
        animate={armed ? { rotate: [0, -16, 12, -8, 4, 0] } : { rotate: 0 }}
        transition={{ duration: 0.5 }}
        className="inline-flex origin-top"
      >
        {armed ? <BellRing aria-hidden="true" /> : <Bell aria-hidden="true" />}
      </m.span>
    </Button>
  )
}

/** A prepaid balance: amount first, the cap if there is one, and its health in one line. */
export const BalanceTile = memo(function BalanceTile({ snapshot }: { snapshot: Snapshot }) {
  const { status } = useNowSelector((now) => effectiveStatus(snapshot, now), shallowEqual)
  const info = providerInfo(snapshot.provider)
  const percent = percentUsed(snapshot)
  const headingId = `balance-${snapshot.id}`.replace(/[^a-z0-9-]/gi, '-')
  return (
    <article aria-labelledby={headingId} className="relative flex h-full flex-col gap-3 rounded-xl border bg-card p-4 hover:border-primary/50">
      <header className="flex items-center gap-3">
        <ProviderMark provider={snapshot.provider} className="size-8 text-xs" />
        <div className="min-w-0 flex-1">
          <h3 id={headingId} className="truncate text-sm leading-tight font-semibold">
            <Link
              to={{ name: 'provider', provider: snapshot.provider, window: snapshot.window }}
              className="outline-none after:absolute after:inset-0 after:rounded-xl focus-visible:after:outline-2 focus-visible:after:outline-offset-2 focus-visible:after:outline-ring"
            >
              {info.name}
            </Link>
          </h3>
          <p className="truncate text-xs text-muted-foreground">{windowTitle(snapshot.provider, snapshot.window)}</p>
        </div>
        {status !== 'ok' && <StatusBadge status={status} />}
      </header>
      {typeof snapshot.remaining !== 'number' && typeof snapshot.used === 'number' ? (
        // Spend with no known allowance (Copilot without one entered): say what was spent, and when the period ends.
        <div className="flex items-baseline justify-between gap-3">
          <p className="num text-2xl leading-none font-semibold">{formatMoney(snapshot.used, snapshot.unit)}</p>
          <p className="num text-right text-xs text-muted-foreground">
            spent
            {parseTime(snapshot.resetsAt) !== undefined && (
              <>
                , resets in <Countdown target={parseTime(snapshot.resetsAt)!} className="font-medium text-foreground" />
              </>
            )}
          </p>
        </div>
      ) : (
        <div className="flex items-baseline justify-between gap-3">
          <p className="num text-2xl leading-none font-semibold">
            {typeof snapshot.remaining === 'number' ? formatMoney(snapshot.remaining, snapshot.unit) : '—'}
          </p>
          <p className="num text-right text-xs text-muted-foreground">
            {typeof snapshot.limit === 'number' ? `left of ${formatMoney(snapshot.limit, snapshot.unit)}` : 'left, no cap'}
          </p>
        </div>
      )}
      {percent !== undefined && typeof snapshot.limit === 'number' && (
        <TideStaff
          percent={percent}
          tone={meterTone(percent)}
          certainty={status === 'ok' ? 'measured' : status === 'unavailable' ? 'unavailable' : 'approximate'}
          label={`${info.name} balance, spent`}
          showScale={false}
          className="[&_[data-slot=progress]]:h-2"
        />
      )}
      <AgeLabel observedAt={snapshot.observedAt} status={status} className="mt-auto" />
    </article>
  )
})
