import { Progress } from '@/components/ui/progress'
import type { MeterTone } from '@/lib/snapshot-view'
import { cn } from '@/lib/utils'

/** How trustworthy the drawn value is. Drawn differently, not only coloured differently. */
export type Certainty = 'measured' | 'approximate' | 'unavailable'

const FILL: Record<Certainty, Record<MeterTone, string>> = {
  measured: { calm: 'bg-tide fill-tide', high: 'bg-shoal fill-shoal', full: 'bg-shoal fill-shoal' },
  // Hatch over a pale fill: an approximate area on a chart.
  approximate: {
    calm: 'bg-tide/30 fill-hatch',
    high: 'bg-shoal/30 fill-hatch',
    full: 'bg-shoal/30 fill-hatch',
  },
  unavailable: {
    calm: 'bg-muted-foreground/20 fill-hatch',
    high: 'bg-muted-foreground/20 fill-hatch',
    full: 'bg-muted-foreground/20 fill-hatch',
  },
}

/**
 * The limit meter, drawn as a graduated tide staff: ticks every 10 percent and a
 * 0 / 50 / 100 scale. Solid fill is measured, hatch is approximate, and a struck
 * line means the reading cannot be refreshed right now (the last value stays). An optional pace
 * marker shows where an even spend across the window would be by now.
 */
export function TideStaff({
  percent,
  tone,
  certainty,
  label,
  showScale = true,
  pace,
  className,
}: {
  percent: number
  tone: MeterTone
  certainty: Certainty
  /** Accessible name, for example "Claude 5-hour session". */
  label: string
  showScale?: boolean
  /** Percent of the window that has passed, drawn as a marker. */
  pace?: number
  className?: string
}) {
  return (
    <div className={className}>
      <Progress
        value={percent}
        aria-label={label}
        aria-valuetext={`${Math.round(percent)} percent used`}
        data-certainty={certainty}
        className={cn(
          'h-3.5 overflow-visible rounded-full bg-track',
          certainty !== 'measured' && 'outline-1 -outline-offset-1 outline-dashed outline-sand-ink/50',
        )}
        indicatorClassName={FILL[certainty][tone]}
      >
        <div aria-hidden="true" className="staff-ticks pointer-events-none absolute inset-0 rounded-full" />
        {pace !== undefined && (
          <div
            aria-hidden="true"
            data-slot="pace-marker"
            className="pointer-events-none absolute -inset-y-1 w-0.5 -translate-x-1/2 rounded-full bg-foreground/70 ring-2 ring-card"
            style={{ left: `${Math.min(100, Math.max(0, pace))}%` }}
          />
        )}
        {certainty === 'unavailable' && (
          <div
            aria-hidden="true"
            className="pointer-events-none absolute inset-0"
            style={{
              background:
                'linear-gradient(to top right, transparent calc(50% - 0.75px), var(--muted-foreground) calc(50% - 0.75px), var(--muted-foreground) calc(50% + 0.75px), transparent calc(50% + 0.75px))',
            }}
          />
        )}
      </Progress>
      {showScale && (
        <div aria-hidden="true" className="num mt-1 flex justify-between text-xs text-muted-foreground">
          <span>0</span>
          <span>50</span>
          <span>100%</span>
        </div>
      )}
    </div>
  )
}
