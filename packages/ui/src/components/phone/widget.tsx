/**
 * Settings for the Android home-screen widget: which limits and balances it shows, and putting it
 * on the home screen. Phone mode only; a source without `phone.widget` shows nothing here.
 *
 * "Show every window" is the default and also covers windows the PC adds later. Turning it off lists
 * every window the phone has, in the order the app lists them, to tick or untick. A change is saved
 * as soon as it is made.
 */
import { LoaderCircle, SquarePlus } from 'lucide-react'
import { useState } from 'react'
import { toast } from 'sonner'
import type { Snapshot } from '@cooldown/snapshot-schema'
import { ErrorState } from '@/components/ScreenStates'
import { Section } from '@/components/SettingsSection'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useSnapshotSource, useSnapshots, useSourceQuery } from '@/data/SnapshotSource'
import type { WidgetControl } from '@/data/SnapshotSource'
import { errorMessage } from '@/hooks/use-phone'
import { providerInfo, windowTitle } from '@/lib/providers'
import { sortSnapshots } from '@/lib/snapshot-view'

/** "Claude · 5-hour session", with the account added only when two windows would read the same. */
export function widgetWindowLabels(snapshots: Snapshot[]): { id: string; label: string }[] {
  const base = (s: Snapshot) => `${providerInfo(s.provider).name} · ${windowTitle(s.provider, s.window)}`
  const counts = new Map<string, number>()
  for (const s of snapshots) counts.set(base(s), (counts.get(base(s)) ?? 0) + 1)
  return snapshots.map((s) => ({
    id: s.id,
    label: (counts.get(base(s)) ?? 0) > 1 && s.account ? `${base(s)} (${s.account})` : base(s),
  }))
}

/** The whole Settings block, or nothing on a phone without a widget. */
export function WidgetSection() {
  const widget = useSnapshotSource().phone?.widget
  if (!widget) return null
  return (
    <Section
      id="widget-heading"
      title="Home-screen widget"
      description="Pick the limits and balances the widget shows. It catches up with your PC about every 15 minutes, even when this app is closed."
    >
      <WidgetChooser widget={widget} />
    </Section>
  )
}

function WidgetChooser({ widget }: { widget: WidgetControl }) {
  const settings = useSourceQuery(() => widget.getSettings())
  const snapshots = useSnapshots()
  const [busy, setBusy] = useState(false)

  if (settings.error && !settings.data) {
    return <ErrorState title="Could not load the widget settings" detail={settings.error.message} onRetry={settings.reload} />
  }
  if (!settings.data) return <Skeleton className="h-40 w-full rounded-xl" />

  const { windows, canPin } = settings.data
  const listed = widgetWindowLabels(sortSnapshots(snapshots.data ?? []))
  const everything = windows === null
  const chosen = new Set(windows ?? listed.map((w) => w.id))
  // Windows on the widget that the PC no longer sends do not count towards "at least one".
  const chosenHere = listed.filter((w) => chosen.has(w.id)).length

  async function save(next: string[] | null) {
    setBusy(true)
    try {
      await widget.setWindows(next)
      settings.reload()
    } catch (err) {
      toast.error('Could not change the widget', { description: errorMessage(err, 'Try again in a moment.') })
    } finally {
      setBusy(false)
    }
  }

  function toggle(id: string, on: boolean) {
    // Keep the list in the app's order whatever order the ticks come in.
    void save(listed.filter((w) => (w.id === id ? on : chosen.has(w.id))).map((w) => w.id))
  }

  async function pin() {
    setBusy(true)
    try {
      if (await widget.pin()) {
        toast('Choose a spot for the widget', { description: 'Android is asking where to put it.' })
      } else {
        toast('Your launcher cannot add it from here', {
          description: 'Press and hold the home screen, choose Widgets, then Cooldown.',
        })
      }
    } catch (err) {
      toast.error('Could not add the widget', { description: errorMessage(err, 'Try again in a moment.') })
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex flex-col gap-4 rounded-xl border bg-card p-4">
      <div className="flex items-center justify-between gap-4">
        <label htmlFor="widget-everything" className="min-w-0 flex-1 cursor-pointer">
          <span className="block text-base font-medium">Show every window</span>
          <span className="block text-sm text-muted-foreground">Includes windows your PC adds later.</span>
        </label>
        <Switch
          id="widget-everything"
          checked={everything}
          disabled={busy || (everything && listed.length === 0)}
          onCheckedChange={(on) => void save(on ? null : listed.map((w) => w.id))}
        />
      </div>

      {everything && listed.length === 0 && (
        <p className="text-sm text-muted-foreground">Windows you can pick from appear here after your PC has synced.</p>
      )}

      {!everything && (
        <fieldset className="flex flex-col" disabled={busy}>
          <legend className="sr-only">Windows on the widget</legend>
          <ul className="divide-y rounded-lg border">
            {listed.map((w) => {
              const on = chosen.has(w.id)
              const last = on && chosenHere <= 1
              return (
                <li key={w.id}>
                  <label className="flex min-h-11 cursor-pointer items-center gap-3 px-3 py-2 has-[:disabled]:cursor-not-allowed">
                    <input
                      type="checkbox"
                      className="size-5 shrink-0 accent-primary"
                      checked={on}
                      disabled={last}
                      onChange={(e) => toggle(w.id, e.target.checked)}
                    />
                    <span className="min-w-0 flex-1 text-base" translate="no">
                      {w.label}
                    </span>
                  </label>
                </li>
              )
            })}
          </ul>
          <p className="mt-2 text-sm text-muted-foreground">The widget needs at least one window.</p>
        </fieldset>
      )}

      {canPin ? (
        <Button variant="outline" disabled={busy} onClick={() => void pin()} className="w-full sm:w-fit">
          {busy ? <LoaderCircle aria-hidden="true" className="animate-spin" /> : <SquarePlus aria-hidden="true" />}
          Add widget to home screen
        </Button>
      ) : (
        <p className="text-sm text-muted-foreground">
          To add the widget, press and hold the home screen, choose Widgets, then Cooldown.
        </p>
      )}
    </div>
  )
}
