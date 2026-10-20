/**
 * Android permissions the phone app needs, said in terms of what the user loses without them.
 * `PermissionsCard` is the full list (Settings); `PermissionNudge` is the short reminder shown
 * where the missing permission hurts (Alerts, right after pairing).
 */
import { CircleCheck, LoaderCircle, TriangleAlert } from 'lucide-react'
import { toast } from 'sonner'
import { ErrorState } from '@/components/ScreenStates'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  errorMessage,
  missingPermissions,
  PERMISSION_KEYS,
  usePhonePermissions,
  type PermissionKey,
} from '@/hooks/use-phone'
import { cn } from '@/lib/utils'
import { Link } from '@/state/router'

const COPY: Record<PermissionKey, { title: string; on: string; off: string; consequence: string }> = {
  exactAlarms: {
    title: 'Exact alarms',
    on: 'A bell rings at the reset time.',
    off: 'Without it Android may ring a bell minutes late.',
    consequence: 'bells may ring late',
  },
  notifications: {
    title: 'Notifications',
    on: 'A bell can show up on your screen.',
    off: 'Without it a bell cannot show up, so you would not know your limit is back.',
    consequence: 'bells cannot show up',
  },
  localNetwork: {
    title: 'Local network',
    on: 'This phone can reach your PC on your Wi-Fi.',
    off: 'Android 17 asks before an app can reach other devices on your Wi-Fi. Without it this phone cannot sync with your PC.',
    consequence: 'this phone cannot reach your PC',
  },
}

/** "a", "a and b", "a, b and c". */
function joinList(items: string[]): string {
  if (items.length <= 1) return items.join('')
  return `${items.slice(0, -1).join(', ')} and ${items[items.length - 1]}`
}

/** Ask Android, then say what is left. Shared by the card and the nudge. */
function useGrant() {
  const { ask, asking, ...rest } = usePhonePermissions()
  async function grant() {
    try {
      const next = await ask()
      const left = missingPermissions(next)
      if (left.length === 0) {
        toast.success('All set', { description: 'Bells will ring on time.' })
      } else {
        toast('Finish in Android settings', {
          description: `Switch on ${joinList(left.map((k) => COPY[k].title.toLowerCase()))}, then come back to Cooldown.`,
        })
      }
    } catch (err) {
      toast.error('Could not ask Android', { description: errorMessage(err, 'Open Android settings and allow Cooldown there.') })
    }
  }
  return { grant, asking, ...rest }
}

function GrantButton({ onClick, asking, className }: { onClick: () => void; asking: boolean; className?: string }) {
  return (
    // aria-disabled, not disabled: a disabled button loses keyboard focus.
    <Button onClick={() => !asking && onClick()} aria-disabled={asking} className={className}>
      {asking && <LoaderCircle aria-hidden="true" className="animate-spin" />}
      Grant permissions
    </Button>
  )
}

export function PermissionsCard() {
  const { permissions, error, asking, grant, refresh, missing } = useGrant()

  if (error && !permissions) return <ErrorState title="Could not check the permissions" detail={error.message} onRetry={refresh} />
  if (!permissions) return <Skeleton className="h-64 w-full rounded-xl" />

  return (
    <div className="flex flex-col gap-4 rounded-xl border bg-card p-4">
      <ul className="flex flex-col divide-y">
        {PERMISSION_KEYS.map((key) => {
          const allowed = permissions[key]
          const Icon = allowed ? CircleCheck : TriangleAlert
          return (
            <li key={key} className="flex items-start gap-3 py-3 first:pt-0 last:pb-0">
              <Icon aria-hidden="true" className={cn('mt-0.5 size-5 shrink-0', allowed ? 'text-tide-ink' : 'text-sand-ink')} />
              <div className="min-w-0 flex-1">
                <p className="flex flex-wrap items-baseline justify-between gap-x-3">
                  <span className="text-base font-medium">{COPY[key].title}</span>
                  <span className={cn('text-sm font-medium', allowed ? 'text-tide-ink' : 'text-sand-ink')}>
                    {allowed ? 'Allowed' : 'Off'}
                  </span>
                </p>
                <p className="mt-0.5 text-sm text-muted-foreground">{allowed ? COPY[key].on : COPY[key].off}</p>
              </div>
            </li>
          )
        })}
      </ul>
      {missing.length > 0 ? (
        <GrantButton onClick={() => void grant()} asking={asking} className="w-full" />
      ) : (
        <p className="text-sm text-muted-foreground">Everything Cooldown needs is allowed.</p>
      )}
    </div>
  )
}

/** A short reminder with the fix beside it. Renders nothing while everything is allowed (or unknown). */
export function PermissionNudge({ className }: { className?: string }) {
  const { asking, grant, missing } = useGrant()
  if (missing.length === 0) return null
  const bellsAffected = missing.some((k) => k !== 'localNetwork')
  return (
    <section
      aria-labelledby="permission-nudge-heading"
      className={cn('flex flex-col gap-3 rounded-xl bg-sand-soft p-4 text-sand-ink', className)}
    >
      <div className="flex items-start gap-3">
        <TriangleAlert aria-hidden="true" className="mt-0.5 size-5 shrink-0" />
        <div className="space-y-1">
          <h2 id="permission-nudge-heading" className="text-base font-semibold">
            {bellsAffected ? 'Bells may be late or silent' : 'This phone may not reach your PC'}
          </h2>
          <p className="text-sm">
            Android has these off: {joinList(missing.map((k) => COPY[k].title.toLowerCase()))}. That means{' '}
            {joinList(missing.map((k) => COPY[k].consequence))}.
          </p>
        </div>
      </div>
      <div className="flex flex-wrap gap-2">
        <GrantButton onClick={() => void grant()} asking={asking} />
        <Button asChild variant="ghost">
          <Link to={{ name: 'settings' }}>See details</Link>
        </Button>
      </div>
    </section>
  )
}
