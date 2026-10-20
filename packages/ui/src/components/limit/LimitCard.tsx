/**
 * Provider limit card.
 *
 * Structure adapted from the Watermelon registry's "Credit Usage Card"
 * (https://registry.watermelon.sh/r/credit-usage-card.json: usage figure, a
 * graduated meter, used / left footer, toggle) for percent windows, and from its
 * "Budget Card" (https://registry.watermelon.sh/r/budget-card.json: large
 * balance, spent / remaining row) for balances. Hard-coded colours, 9px labels,
 * mock data and the dropdown / modal chrome were removed; everything draws from
 * the theme tokens and the Snapshot contract.
 */
import { CircleOff, Clock, TriangleAlert } from 'lucide-react'
import { memo, type ComponentType, type SVGProps } from 'react'
import type { Snapshot } from '@cooldown/snapshot-schema'
import { adapterSummary, providerInfo, windowTitle } from '@/lib/providers'
import {
  effectiveStatus,
  formatClock,
  formatMoney,
  isMoneyUnit,
  meterTone,
  parseTime,
  percentUsed,
  windowLabel,
} from '@/lib/snapshot-view'
import { cn } from '@/lib/utils'
import { Link } from '@/state/router'
import { shallowEqual, useNowSelector, useToday } from '@/state/now'
import { AgeLabel, Countdown, ProviderMark, SourceBadge, StatusBadge } from './atoms'
import { BellButton } from './BellButton'
import { TideStaff, type Certainty } from './TideStaff'

const NOTICE_ICON: Record<'stale' | 'degraded' | 'unavailable', ComponentType<SVGProps<SVGSVGElement>>> = {
  stale: Clock,
  degraded: TriangleAlert,
  unavailable: CircleOff,
}

/**
 * Memoised: with the same snapshot object it only renders again when its status flips (a reset passes)
 * or the day changes. Countdown and age labels inside it tick on their own.
 */
export const LimitCard = memo(function LimitCard({
  snapshot,
  linked = true,
  className,
}: {
  snapshot: Snapshot
  /** Overview cards link to their provider detail. The detail screen shows the same card unlinked. */
  linked?: boolean
  className?: string
}) {
  const { status, resetPassed } = useNowSelector((now) => effectiveStatus(snapshot, now), shallowEqual)
  const today = useToday()
  const provider = providerInfo(snapshot.provider)
  const title = windowTitle(snapshot.provider, snapshot.window)
  const isBalance = snapshot.window === 'balance' || isMoneyUnit(snapshot.unit)
  const percent = percentUsed(snapshot)
  const tone = meterTone(percent)
  const resetsAt = parseTime(snapshot.resetsAt)
  const certainty: Certainty =
    status === 'unavailable'
      ? 'unavailable'
      : status !== 'ok' || snapshot.source.kind === 'estimated'
        ? 'approximate'
        : 'measured'
  const headingId = `card-${snapshot.id.replace(/[^a-z0-9]/gi, '-')}`
  const meterLabel = `${provider.name} ${title}, ${isBalance ? 'spent' : 'used'}`
  const limitReached = !isBalance && percent !== undefined && percent >= 100 && !resetPassed

  // Something is wrong: say what, in a tinted box. Otherwise one quiet line on where the number comes from.
  const problem =
    status !== 'ok'
      ? resetPassed && status === 'stale'
        ? 'This window has reset. Waiting for a fresh reading; the figure below is from before the reset.'
        : (snapshot.note ?? STATUS_FALLBACK[status])
      : undefined
  const quietLine = snapshot.note ?? adapterSummary(snapshot.source.adapter)

  return (
    <article
      aria-labelledby={headingId}
      data-status={status}
      data-tone={tone}
      className={cn(
        'relative flex flex-col gap-4 rounded-xl border bg-card p-4 text-card-foreground transition-colors sm:p-5',
        linked && 'hover:border-primary/50',
        limitReached && 'border-shoal/60',
        className,
      )}
    >
      <header className="flex items-start gap-3">
        <ProviderMark provider={snapshot.provider} />
        <div className="min-w-0 flex-1">
          <h2 id={headingId} className="truncate text-base leading-tight font-semibold">
            {linked ? (
              <Link
                to={{ name: 'provider', provider: snapshot.provider, window: snapshot.window }}
                className="outline-none after:absolute after:inset-0 after:rounded-xl focus-visible:after:outline-2 focus-visible:after:outline-offset-2 focus-visible:after:outline-ring"
              >
                {provider.name}
              </Link>
            ) : (
              provider.name
            )}
          </h2>
          <p className="text-sm text-muted-foreground">
            {title}
            {snapshot.account ? <span> · {snapshot.account}</span> : null}
          </p>
        </div>
        <SourceBadge kind={snapshot.source.kind} />
      </header>

      {problem ? (
        <p
          className={cn(
            'flex items-start gap-2 rounded-md px-3 py-2 text-sm',
            status === 'unavailable' ? 'bg-muted text-muted-foreground' : 'bg-sand-soft text-sand-ink',
          )}
        >
          {status !== 'ok' && <NoticeIcon status={status} />}
          <span>{problem}</span>
        </p>
      ) : (
        <p className="text-sm text-muted-foreground">{quietLine}</p>
      )}

      <div className="grid grid-cols-[minmax(0,1fr)_auto] items-end gap-x-4 gap-y-1">
        <div className="min-w-0">
          {isBalance ? (
            <>
              <p className="num text-3xl leading-none font-semibold">
                {typeof snapshot.remaining === 'number' ? formatMoney(snapshot.remaining, snapshot.unit) : 'No reading'}
              </p>
              <p className="mt-1.5 text-sm text-muted-foreground">
                {typeof snapshot.limit === 'number' ? (
                  <>
                    left of <span className="num">{formatMoney(snapshot.limit, snapshot.unit)}</span>
                  </>
                ) : (
                  'left, no spending cap'
                )}
              </p>
            </>
          ) : (
            <>
              <p className="num text-4xl leading-none font-semibold">
                {percent === undefined ? '—' : Math.round(percent)}
                {percent !== undefined && <span className="ml-0.5 text-2xl font-medium text-muted-foreground">%</span>}
              </p>
              <p className={cn('mt-1.5 text-sm', limitReached ? 'font-medium text-shoal-ink' : 'text-muted-foreground')}>
                {percent === undefined ? 'no reading yet' : limitReached ? 'Limit reached' : 'used'}
              </p>
            </>
          )}
        </div>

        <div className="min-w-0 text-right">
          <ResetBlock
            resetsAt={resetsAt}
            resetPassed={resetPassed}
            isBalance={isBalance}
            limitReached={limitReached}
            today={today}
          />
        </div>
      </div>

      {percent !== undefined ? (
        <TideStaff percent={percent} tone={tone} certainty={certainty} label={meterLabel} />
      ) : isBalance ? (
        <p className="text-sm text-muted-foreground">Prepaid balance with no cap to measure against.</p>
      ) : null}

      {isBalance && typeof snapshot.used === 'number' && (
        <dl className="num -mt-1 flex justify-between text-sm">
          <div>
            <dt className="text-muted-foreground">Spent</dt>
            <dd className="font-medium">{formatMoney(snapshot.used, snapshot.unit)}</dd>
          </div>
          {percent !== undefined && (
            <div className="text-right">
              <dt className="text-muted-foreground">Of limit</dt>
              <dd className="font-medium">{Math.round(percent)}%</dd>
            </div>
          )}
        </dl>
      )}

      <footer className="mt-auto flex flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1">
          <StatusBadge status={status} />
          <AgeLabel observedAt={snapshot.observedAt} status={status} />
        </div>
        {isBalance ? (
          <p className="text-xs text-muted-foreground">
            {windowLabel(snapshot.window)} figures do not reset, so there is no bell to set.
          </p>
        ) : resetsAt === undefined ? (
          <p className="text-xs text-muted-foreground">The bell is available once the reset time is known.</p>
        ) : (
          <div className="relative z-10">
            <BellButton snapshot={snapshot} />
          </div>
        )}
      </footer>
    </article>
  )
})

const STATUS_FALLBACK = {
  stale: 'This reading has not been refreshed lately. The last good value is shown.',
  degraded: 'The adapter is struggling, so this value may be off.',
  unavailable: 'The adapter cannot read this right now. The last good value is shown.',
} as const

function NoticeIcon({ status }: { status: 'stale' | 'degraded' | 'unavailable' }) {
  const Icon = NOTICE_ICON[status]
  return <Icon aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
}

function ResetBlock({
  resetsAt,
  resetPassed,
  isBalance,
  limitReached,
  today,
}: {
  resetsAt: number | undefined
  resetPassed: boolean
  isBalance: boolean
  limitReached: boolean
  /** Start of today: `formatClock` only needs the day. */
  today: number
}) {
  if (isBalance) {
    return (
      <>
        <p className="text-lg leading-none font-semibold text-muted-foreground">No reset</p>
        <p className="mt-1.5 text-sm text-muted-foreground">balance only</p>
      </>
    )
  }
  if (resetsAt === undefined) {
    return (
      <>
        <p className="text-sm text-muted-foreground">Resets</p>
        <p className="mt-1.5 text-lg leading-none font-semibold text-muted-foreground">time not known yet</p>
      </>
    )
  }
  if (resetPassed) {
    return (
      <>
        <p className="text-lg leading-none font-semibold text-sand-ink">Reset passed</p>
        <p className="num mt-1.5 text-sm text-muted-foreground">{formatClock(resetsAt, today)}</p>
      </>
    )
  }
  return (
    <>
      <p className="text-sm text-muted-foreground">{limitReached ? 'Back in' : 'Resets in'}</p>
      <Countdown target={resetsAt} className={cn('block text-2xl leading-tight font-semibold', limitReached && 'text-shoal-ink')} />
      <p className="num text-sm text-muted-foreground">at {formatClock(resetsAt, today)}</p>
    </>
  )
}
