import { Calculator, CircleCheck, CircleOff, Clock, PenLine, ScanSearch, ShieldCheck, TriangleAlert } from 'lucide-react'
import type { ComponentType, CSSProperties, SVGProps } from 'react'
import type { SourceKind, Status } from '@cooldown/snapshot-schema'
import { Badge } from '@/components/ui/badge'
import { cn } from '@/lib/utils'
import { providerInfo, SOURCE_COPY, STATUS_COPY } from '@/lib/providers'
import { countdownSpoken, formatAge, formatCountdown, formatFull, OLD_OBSERVATION_MS, parseTime } from '@/lib/snapshot-view'
import { shallowEqual, useNowSelector } from '@/state/now'

type IconType = ComponentType<SVGProps<SVGSVGElement>>

/** Monogram tile in the provider's own hue. Providers' own logos are deliberately not used. */
export function ProviderMark({ provider, className }: { provider: string; className?: string }) {
  const { mark, hue } = providerInfo(provider)
  return (
    <span
      aria-hidden="true"
      data-neutral={hue === null ? '' : undefined}
      style={hue === null ? undefined : ({ '--mark-h': hue } as CSSProperties)}
      className={cn(
        'provider-mark grid size-10 shrink-0 place-items-center rounded-lg border text-sm font-semibold',
        className,
      )}
    >
      {mark}
    </span>
  )
}

const SOURCE_STYLE: Record<SourceKind, { icon: IconType; className: string }> = {
  // Surveyed: solid outline.
  official: { icon: ShieldCheck, className: 'border-solid border-tide/70 bg-tide-soft text-tide-ink' },
  // Approximate and undocumented: dashed and dotted outlines, the way a chart marks them.
  estimated: { icon: Calculator, className: 'border-dashed border-sand-ink/60 bg-sand-soft text-sand-ink' },
  scraped: { icon: ScanSearch, className: 'border-dotted border-sand-ink/70 bg-sand-soft text-sand-ink' },
  manual: { icon: PenLine, className: 'border-solid border-border bg-secondary text-secondary-foreground' },
}

/** Where a number came from. The outline style carries certainty so it survives without colour. */
export function SourceBadge({ kind, className }: { kind: SourceKind; className?: string }) {
  const { icon: Icon, className: style } = SOURCE_STYLE[kind]
  const copy = SOURCE_COPY[kind]
  return (
    <Badge
      variant="outline"
      title={`${copy.label}. ${copy.meaning}`}
      data-source={kind}
      className={cn('h-6 rounded-md px-2', style, className)}
    >
      <Icon aria-hidden="true" />
      <span className="sr-only">Source: </span>
      {copy.label}
    </Badge>
  )
}

const STATUS_STYLE: Record<Status, { icon: IconType; className: string }> = {
  ok: { icon: CircleCheck, className: 'border-transparent bg-transparent text-tide-ink' },
  stale: { icon: Clock, className: 'border-dashed border-sand-ink/60 bg-sand-soft text-sand-ink' },
  degraded: { icon: TriangleAlert, className: 'border-solid border-sand-ink/70 bg-sand-soft text-sand-ink' },
  unavailable: { icon: CircleOff, className: 'border-solid border-border bg-muted text-muted-foreground' },
}

export function StatusBadge({ status, className }: { status: Status; className?: string }) {
  const { icon: Icon, className: style } = STATUS_STYLE[status]
  return (
    <Badge
      variant="outline"
      title={STATUS_COPY[status].meaning}
      data-status={status}
      className={cn('h-6 rounded-md px-2', status === 'ok' && 'px-0', style, className)}
    >
      <Icon aria-hidden="true" />
      <span className="sr-only">Status: </span>
      {STATUS_COPY[status].label}
    </Badge>
  )
}

/** "Last observed 2 min ago". Flags a reading that is old even when the adapter still says ok. */
export function AgeLabel({
  observedAt,
  status,
  className,
  prefix = true,
}: {
  observedAt: string
  status: Status
  className?: string
  /** Set false where a "Last observed" row label already says it. */
  prefix?: boolean
}) {
  const observed = parseTime(observedAt)
  // Re-renders when the words change ("just now", "2 min ago", ...) or the reading turns old, not every second.
  const { label, old } = useNowSelector((now) => {
    const ageMs = observed === undefined ? Number.POSITIVE_INFINITY : Math.max(0, now - observed)
    return { label: formatAge(ageMs), old: ageMs > OLD_OBSERVATION_MS || status !== 'ok' }
  }, shallowEqual)
  return (
    <span className={cn('inline-flex items-center gap-1 text-xs', old ? 'text-sand-ink' : 'text-muted-foreground', className)}>
      {prefix && <>Last observed </>}
      <time dateTime={observedAt} title={observed === undefined ? undefined : formatFull(observed)} className="num font-medium">
        {label}
      </time>
    </span>
  )
}

/**
 * Live countdown. Visible text ticks every second below one hour and once a minute above it; assistive
 * tech is not interrupted by it. This is the one component that re-renders every second.
 */
export function Countdown({ target, className }: { target: number; className?: string }) {
  const { text, spoken } = useNowSelector((now) => ({ text: formatCountdown(target, now), spoken: countdownSpoken(target, now) }), shallowEqual)
  return (
    <span role="timer" aria-live="off" aria-label={spoken} className={cn('num num-ticking whitespace-nowrap', className)}>
      {text}
    </span>
  )
}
