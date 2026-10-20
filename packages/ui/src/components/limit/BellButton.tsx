import { Bell, BellRing } from 'lucide-react'
import * as m from 'motion/react-m'
import type { Snapshot } from '@cooldown/snapshot-schema'
import { Button } from '@/components/ui/button'
import { canAlert, formatClock, parseTime } from '@/lib/snapshot-view'
import { cn } from '@/lib/utils'
import { useAlerts } from '@/state/alerts'
import { useNowSelector, useToday } from '@/state/now'
import { Countdown } from './atoms'

/**
 * "Notify me when it resets". UI state only: it arms or removes an AlertIntent
 * in local state and shows the time the bell will ring.
 */
export function BellButton({
  snapshot,
  className,
  size = 'default',
}: {
  snapshot: Snapshot
  className?: string
  size?: 'default' | 'lg'
}) {
  const { isArmed, toggle, alerts } = useAlerts()
  const today = useToday()
  const canSetNow = useNowSelector((now) => canAlert(snapshot, now))
  const armed = isArmed(snapshot)
  const eligible = armed || canSetNow
  const intent = alerts.find((a) => a.id === `reset:${snapshot.provider}:${snapshot.account}:${snapshot.window}`)
  const fireAt = parseTime(intent?.fireAt)
  // A bell whose time has passed (the phone may not have heard from the PC since) says it rang.
  const rang = useNowSelector((now) => fireAt !== undefined && fireAt <= now)

  return (
    <div className={cn('flex flex-col gap-1.5', className)}>
      <Button
        type="button"
        size={size}
        variant={armed ? 'default' : 'outline'}
        aria-pressed={armed}
        disabled={!eligible}
        onClick={() => toggle(snapshot)}
        className="h-auto min-h-11 w-full justify-start py-2 text-left whitespace-normal lg:min-h-9 lg:py-1.5"
      >
        {/* initial={false}: the bell only rocks when the user turns it on, never on first paint. */}
        <m.span
          initial={false}
          animate={armed ? { rotate: [0, -16, 12, -8, 4, 0] } : { rotate: 0 }}
          transition={{ duration: 0.5 }}
          className="inline-flex origin-top"
        >
          {armed ? <BellRing aria-hidden="true" /> : <Bell aria-hidden="true" />}
        </m.span>
        Notify me when it resets
      </Button>
      <p className="min-h-4 text-xs text-muted-foreground">
        {armed && fireAt !== undefined && rang ? (
          <>
            Bell rang at <span className="num font-medium text-foreground">{formatClock(fireAt, today)}</span>
          </>
        ) : armed && fireAt !== undefined ? (
          <>
            Bell rings at <span className="num font-medium text-foreground">{formatClock(fireAt, today)}</span>, in{' '}
            <Countdown target={fireAt} className="font-medium text-foreground" />
          </>
        ) : !eligible ? (
          'Waiting for a fresh reading before a bell can be set.'
        ) : null}
      </p>
    </div>
  )
}
