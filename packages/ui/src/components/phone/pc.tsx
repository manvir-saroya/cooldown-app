/**
 * The paired PC as the phone sees it: connection state, reconnect, and the note shown when the PC cannot be reached.
 */
import { LoaderCircle, QrCode, RefreshCw, Wifi, WifiOff } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { EmptyState } from '@/components/ScreenStates'
import { Button } from '@/components/ui/button'
import { useSnapshotSource } from '@/data/SnapshotSource'
import type { PairedPc } from '@/data/types'
import { errorMessage, usePairedPcs } from '@/hooks/use-phone'
import { parseTime } from '@/lib/snapshot-view'
import { cn } from '@/lib/utils'
import { useAge } from '@/state/now'
import { Link } from '@/state/router'

/** Runs `phone.reconnect()` and reports the outcome. */
export function useReconnect(onDone?: () => void) {
  const phone = useSnapshotSource().phone
  const [busy, setBusy] = useState(false)
  async function reconnect() {
    if (!phone) return
    setBusy(true)
    try {
      await phone.reconnect()
      toast.success('Connected to your PC')
    } catch (err) {
      toast.error('Could not reach your PC', {
        description: errorMessage(err, 'Check that it is on and on the same Wi-Fi, then try again.'),
      })
    } finally {
      setBusy(false)
      onDone?.()
    }
  }
  return { reconnect, busy }
}

export function ReconnectButton({
  onDone,
  variant = 'outline',
  className,
}: {
  onDone?: () => void
  variant?: 'default' | 'outline'
  className?: string
}) {
  const { reconnect, busy } = useReconnect(onDone)
  return (
    // aria-disabled, not disabled: a disabled button loses keyboard focus.
    <Button variant={variant} onClick={() => !busy && void reconnect()} aria-disabled={busy} className={className}>
      {busy ? <LoaderCircle aria-hidden="true" className="animate-spin" /> : <RefreshCw aria-hidden="true" />}
      Reconnect
    </Button>
  )
}

/** Icon and word, never colour alone. Two short lines so a long age never wraps in an odd place. */
export function ConnectionState({ pc, className }: { pc: PairedPc; className?: string }) {
  const seen = parseTime(pc.lastSeenAt)
  const seenAge = useAge(seen)
  const Icon = pc.connected ? Wifi : WifiOff
  return (
    <div className={cn('num text-sm', className)}>
      <p className={cn('flex items-center gap-1.5 font-medium', pc.connected ? 'text-tide-ink' : 'text-sand-ink')}>
        <Icon aria-hidden="true" className="size-4" />
        {pc.connected ? 'Connected' : 'Not connected'}
      </p>
      <p className="text-muted-foreground">
        {seen === undefined ? 'Never synced' : `Last synced ${seenAge}`}
      </p>
    </div>
  )
}

/** Shown on the overview only when a PC is paired but cannot be reached, so old readings are not mistaken for live ones. */
export function PcStatusNote() {
  const pcs = usePairedPcs()
  const list = pcs.data
  const lastSeen = Math.max(...(list ?? []).map((p) => parseTime(p.lastSeenAt) ?? Number.NEGATIVE_INFINITY))
  const lastSeenAge = useAge(Number.isFinite(lastSeen) ? lastSeen : undefined)
  if (!list || list.length === 0 || list.some((p) => p.connected)) return null
  return (
    <section
      role="status"
      aria-label="Connection to your PC"
      className="flex flex-col gap-3 rounded-xl bg-sand-soft p-4 text-sm text-sand-ink"
    >
      <p className="flex items-start gap-3">
        <WifiOff aria-hidden="true" className="mt-0.5 size-5 shrink-0" />
        <span>
          <span className="font-semibold">{list.length === 1 ? list[0]?.name : 'Your PC'} is not connected.</span> These readings are from the
          last sync{Number.isFinite(lastSeen) ? `, ${lastSeenAge}` : ''}. Bells already set still ring.
        </span>
      </p>
      <ReconnectButton onDone={pcs.reload} />
    </section>
  )
}

/** The overview when the phone holds no readings: either nothing is paired yet, or the first sync has not arrived. */
export function NoReadingsYet() {
  const pcs = usePairedPcs()
  const paired = (pcs.data ?? []).length > 0
  return (
    <EmptyState
      icon={QrCode}
      title={paired ? 'Waiting for your PC' : 'No limits yet'}
      action={
        paired ? (
          <ReconnectButton onDone={pcs.reload} className="w-full sm:w-auto" />
        ) : (
          <Button asChild className="w-full sm:w-auto">
            <Link to={{ name: 'pairing' }}>Pair with your PC</Link>
          </Button>
        )
      }
    >
      {paired
        ? 'This phone is paired, but your PC has not sent any readings yet. Check that Cooldown is running on it and that both are on the same Wi-Fi.'
        : 'This phone shows the limits your PC reads. Pair with the PC that runs Cooldown and they appear here, with a bell for each reset.'}
    </EmptyState>
  )
}
