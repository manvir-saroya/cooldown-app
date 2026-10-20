/**
 * Settings when the app runs on a phone. The PC is the one that reads providers and keeps keys and
 * bells, so there are no API keys and no Claude hook here: this shows which PC the phone follows,
 * whether it is connected, and the Android permissions that make bells ring on time.
 */
import { Lock, Monitor } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import { ConnectionState, ReconnectButton } from '@/components/phone/pc'
import { PermissionsCard } from '@/components/phone/permissions'
import { WidgetSection } from '@/components/phone/widget'
import { ErrorState } from '@/components/ScreenStates'
import { AppearanceSection, NotificationsSection, Section } from '@/components/SettingsSection'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { useSnapshotSource } from '@/data/SnapshotSource'
import type { PairedPc } from '@/data/types'
import { errorMessage, usePairedPcs } from '@/hooks/use-phone'
import { Link } from '@/state/router'

export function PhoneSettingsScreen() {
  return (
    <div className="flex max-w-2xl flex-col gap-8">
      <PcSection />
      <NotificationsSection />
      <WidgetSection />
      <Section
        id="permissions-heading"
        title="Permissions"
        description="Android asks before an app can ring on time, show a notification or talk to your PC. Here is what each one is for."
      >
        <PermissionsCard />
      </Section>
      <p className="flex items-start gap-2.5 rounded-lg bg-secondary p-3 text-sm text-secondary-foreground">
        <Lock aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
        <span>
          <strong className="font-semibold">Keys stay on your PC.</strong> API keys and the Claude Code hook are set up
          in Cooldown on your PC. This phone only receives readings and bell times, never a key.
        </span>
      </p>
      <AppearanceSection />
    </div>
  )
}

function PcSection() {
  const pcs = usePairedPcs()
  return (
    <Section
      id="pc-heading"
      title="Your PC"
      description="Bells are kept on your PC. This phone shows them and rings them on time, even when the app is closed."
    >
      {pcs.error && !pcs.data ? (
        <ErrorState title="Could not load your PC" detail={pcs.error.message} onRetry={pcs.reload} />
      ) : !pcs.data ? (
        <Skeleton className="h-40 w-full rounded-xl" />
      ) : pcs.data.length === 0 ? (
        <div className="flex flex-col items-start gap-3 rounded-xl border border-dashed bg-card p-5">
          <p className="text-sm text-muted-foreground">
            No PC is paired. Pair with the PC that runs Cooldown to get its limits and bells here.
          </p>
          <Button asChild className="w-full sm:w-auto">
            <Link to={{ name: 'pairing' }}>Pair with your PC</Link>
          </Button>
        </div>
      ) : (
        <>
          <ul className="divide-y rounded-xl border bg-card">
            {pcs.data.map((pc) => (
              <PcRow key={pc.id} pc={pc} onChanged={pcs.reload} />
            ))}
          </ul>
          <div>
            <Button asChild variant="outline" className="w-full sm:w-auto">
              <Link to={{ name: 'pairing' }}>Pair a different PC</Link>
            </Button>
          </div>
        </>
      )}
    </Section>
  )
}

function PcRow({ pc, onChanged }: { pc: PairedPc; onChanged: () => void }) {
  const source = useSnapshotSource()
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState(false)

  async function unpair() {
    if (!source.phone) return
    setBusy(true)
    try {
      await source.phone.unpair(pc.id)
      toast(`${pc.name} unpaired`, { description: 'This phone no longer gets its readings or bells.' })
      setConfirm(false)
      onChanged()
    } catch (err) {
      toast.error('Could not unpair', { description: errorMessage(err, 'Try again in a moment.') })
    } finally {
      setBusy(false)
    }
  }

  return (
    <li className="flex flex-col gap-3 p-4">
      <div className="flex items-center gap-3">
        <span aria-hidden="true" className="grid size-10 shrink-0 place-items-center rounded-lg bg-secondary text-secondary-foreground">
          <Monitor className="size-5" />
        </span>
        <div className="min-w-0 flex-1">
          <p className="truncate text-base font-medium" translate="no">
            {pc.name}
          </p>
          <ConnectionState pc={pc} />
        </div>
      </div>
      <div className="grid grid-cols-2 gap-2">
        <ReconnectButton variant={pc.connected ? 'outline' : 'default'} onDone={onChanged} />
        <Button variant="outline" onClick={() => setConfirm(true)} aria-label={`Unpair ${pc.name}`}>
          Unpair
        </Button>
      </div>
      <Dialog open={confirm} onOpenChange={setConfirm}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Unpair {pc.name}?</DialogTitle>
            <DialogDescription>
              This phone forgets the PC and stops getting its readings and bells. Nothing is deleted on the PC. To
              follow it again, scan a new code from the PC.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex-col-reverse gap-2 sm:flex-row sm:justify-end">
            <DialogClose asChild>
              <Button variant="outline">Keep pairing</Button>
            </DialogClose>
            <Button variant="destructive" disabled={busy} onClick={() => void unpair()}>
              Unpair {pc.name}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </li>
  )
}
