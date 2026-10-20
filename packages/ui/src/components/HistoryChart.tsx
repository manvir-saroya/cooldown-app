import { useReducedMotion } from 'motion/react'
import { useMemo, useState } from 'react'
import { Area, AreaChart, CartesianGrid, ReferenceLine, XAxis, YAxis } from 'recharts'
import type { Snapshot } from '@cooldown/snapshot-schema'
import { ChartContainer, ChartTooltip, ChartTooltipContent, type ChartConfig } from '@/components/ui/chart'
import { Skeleton } from '@/components/ui/skeleton'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { useSourceQuery } from '@/data/SnapshotSource'
import { HISTORY_RANGES, type HistoryRange, type HistorySeries } from '@/data/types'
import { formatClock, formatMoney, historyTitle } from '@/lib/snapshot-view'
import { useToday } from '@/state/now'
import { ErrorState } from './ScreenStates'

const hourFormat = new Intl.DateTimeFormat(undefined, { hour: '2-digit', minute: '2-digit' })
const dayFormat = new Intl.DateTimeFormat(undefined, { weekday: 'short' })
const weekdayDateFormat = new Intl.DateTimeFormat(undefined, { weekday: 'short', day: 'numeric' })
const dateFormat = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' })

/**
 * The label under one x-axis tick. A full 24-hour range starts and ends at the same clock time, and a
 * full 7-day range on the same weekday, so those labels would read the same at both ends. Ticks that
 * fall on a later day than `start` say which day, and the weekly range adds the day of the month.
 */
export function tickLabel(range: HistoryRange, t: number, start?: number): string {
  if (range === '24h') {
    const time = hourFormat.format(t)
    return start !== undefined && new Date(t).toDateString() !== new Date(start).toDateString()
      ? `${dayFormat.format(t)} ${time}`
      : time
  }
  return range === '7d' ? weekdayDateFormat.format(t) : dateFormat.format(t)
}

export function evenTicks(from: number, to: number, count: number): number[] {
  const step = (to - from) / (count - 1)
  return Array.from({ length: count }, (_, i) => Math.round(from + step * i))
}

/** One sentence that says what the chart shows, for people who cannot see it. */
function summarize(series: HistorySeries, now: number): string {
  const values = series.points.map((p) => p.value)
  if (values.length === 0) return 'No history yet.'
  if (series.kind === 'balance') {
    const first = values[0] ?? 0
    const last = values[values.length - 1] ?? 0
    return `Balance went from ${formatMoney(first, series.unit)} to ${formatMoney(last, series.unit)} over this range.`
  }
  const peak = Math.max(...values)
  const peakPoint = series.points.find((p) => p.value === peak)
  const resets = series.resets.length
  return `Highest use was ${Math.round(peak)} percent${peakPoint ? ` at ${formatClock(peakPoint.t, now)}` : ''}. ${resets} ${resets === 1 ? 'reset' : 'resets'} in this range.`
}

export function HistoryChart({ snapshot }: { snapshot: Snapshot }) {
  const defaultRange: HistoryRange = snapshot.window === 'session_5h' ? '24h' : snapshot.window === 'weekly' ? '7d' : '30d'
  const [range, setRange] = useState<HistoryRange>(defaultRange)
  const reduceMotion = useReducedMotion()
  const today = useToday()
  const query = useSourceQuery((s) => s.getHistory(snapshot.id, range), [snapshot.id, range])
  const series = query.data
  const isBalance = series?.kind === 'balance'

  const config: ChartConfig = {
    value: {
      label: isBalance ? 'Balance' : 'Used',
      color: 'var(--tide)',
    },
  }

  const xTicks = useMemo(() => {
    if (!series || series.points.length < 2) return undefined
    const first = series.points[0]
    const last = series.points[series.points.length - 1]
    return first && last ? evenTicks(first.t, last.t, 5) : undefined
  }, [series])

  return (
    <section aria-labelledby={`history-${snapshot.id}`} className="flex min-w-0 flex-col gap-3 rounded-xl border bg-card p-4 sm:p-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h2 id={`history-${snapshot.id}`} className="text-base font-semibold">
          {historyTitle(snapshot)}
        </h2>
        <ToggleGroup
          type="single"
          variant="outline"
          spacing={0}
          value={range}
          onValueChange={(v) => v && setRange(v as HistoryRange)}
          aria-label="History range"
        >
          {HISTORY_RANGES.map((r) => (
            <ToggleGroupItem key={r.value} value={r.value} aria-label={`${r.value} (${r.label})`} className="data-[state=on]:bg-accent data-[state=on]:font-semibold">
              {r.value}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      </div>

      {query.error && !series ? (
        <ErrorState title="Could not load history" detail={query.error.message} onRetry={query.reload} />
      ) : !series ? (
        <Skeleton className="h-56 w-full" />
      ) : (
        <figure className="m-0">
          <ChartContainer config={config} className="aspect-auto h-56 w-full" aria-hidden="true">
            <AreaChart data={series.points} accessibilityLayer={false} margin={{ top: 12, right: 8, bottom: 0, left: 0 }}>
              <CartesianGrid vertical={false} strokeDasharray="2 4" />
              <XAxis
                dataKey="t"
                type="number"
                scale="time"
                domain={['dataMin', 'dataMax']}
                ticks={xTicks}
                tickFormatter={(t: number) => tickLabel(range, t, xTicks?.[0])}
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                interval="preserveStartEnd"
              />
              <YAxis
                width={isBalance ? (series.unit === 'cny' ? 64 : 52) : 40}
                tickLine={false}
                axisLine={false}
                domain={isBalance ? [0, 'auto'] : [0, 100]}
                ticks={isBalance ? undefined : [0, 50, 100]}
                tickFormatter={(v: number) => (isBalance ? formatMoney(v, series.unit, 0) : `${v}%`)}
              />
              {!isBalance && (
                <ReferenceLine
                  y={100}
                  stroke="var(--shoal)"
                  strokeDasharray="4 3"
                  label={{ value: 'Limit', position: 'insideTopRight', fill: 'var(--shoal-ink)', fontSize: 12 }}
                />
              )}
              {series.resets.map((t) => (
                <ReferenceLine key={t} x={t} stroke="var(--muted-foreground)" strokeDasharray="2 4" strokeOpacity={0.6} />
              ))}
              <ChartTooltip
                cursor={{ stroke: 'var(--muted-foreground)', strokeDasharray: '2 2' }}
                content={
                  <ChartTooltipContent
                    indicator="line"
                    labelFormatter={(_, payload) => {
                      const t = payload?.[0]?.payload?.t
                      return typeof t === 'number' ? formatClock(t, today) : ''
                    }}
                    formatter={(value) => (
                      <span className="num font-medium">
                        {isBalance ? formatMoney(Number(value), series.unit) : `${Math.round(Number(value))}% used`}
                      </span>
                    )}
                  />
                }
              />
              <Area
                dataKey="value"
                type="linear"
                stroke="var(--color-value)"
                strokeWidth={2}
                fill="var(--color-value)"
                fillOpacity={0.16}
                dot={false}
                activeDot={{ r: 4 }}
                isAnimationActive={!reduceMotion}
                animationDuration={500}
              />
            </AreaChart>
          </ChartContainer>
          <figcaption className="mt-2 text-sm text-muted-foreground">
            {summarize(series, today)}
            {!isBalance && series.resets.length > 0 && ' Dotted lines mark resets.'}
          </figcaption>
        </figure>
      )}
    </section>
  )
}
