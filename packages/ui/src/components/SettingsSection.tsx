import { Check, Monitor, Moon, Sun } from 'lucide-react'
import { useEffect, useId, useState, type ReactNode } from 'react'
import { toast } from 'sonner'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { useSnapshotSource, useSourceQuery } from '@/data/SnapshotSource'
import type { NotificationSettings } from '@/data/types'
import { useTheme, type ThemePreference } from '@/state/theme'

/** A titled block on the Settings screen (PC and phone). */
export function Section({
  id,
  title,
  description,
  children,
}: {
  id: string
  title: string
  description?: ReactNode
  children: ReactNode
}) {
  return (
    <section aria-labelledby={id} className="flex flex-col gap-3">
      <div className="space-y-1">
        <h2 id={id} className="text-lg font-semibold">
          {title}
        </h2>
        {description && <p className="max-w-prose text-sm text-muted-foreground">{description}</p>}
      </div>
      {children}
    </section>
  )
}

/** Warning levels offered for "Running low". */
const LOW_LEVELS = [75, 80, 90, 95]

/**
 * Automatic notifications. Each switch saves at once; a failed save puts the switch back and says why.
 * Shown only when the source can keep them (`source.notifications`).
 */
export function NotificationsSection() {
  const source = useSnapshotSource()
  const control = source.notifications
  const onPhone = Boolean(source.phone)
  const query = useSourceQuery((s) => s.notifications?.get() ?? Promise.resolve(undefined))
  const [value, setValue] = useState<NotificationSettings | undefined>(undefined)
  useEffect(() => {
    if (query.data) setValue(query.data)
  }, [query.data])
  if (!control) return null

  async function save(next: NotificationSettings) {
    const previous = value
    setValue(next)
    try {
      await control!.set(next)
    } catch (err) {
      setValue(previous)
      toast.error('That setting was not saved', { description: err instanceof Error ? err.message : String(err) })
    }
  }

  return (
    <Section
      id="notifications-heading"
      title="Notifications"
      description={
        onPhone
          ? 'This phone rings for these by itself, from what your PC sends it. Bells for when a limit comes back are set on the PC.'
          : 'Cooldown rings on this PC, and on paired phones for limits that come back. You can still set a bell by hand on any limit.'
      }
    >
      {value === undefined ? (
        query.error ? (
          <p className="text-sm text-sand-ink">These settings could not be read. {query.error.message}</p>
        ) : (
          <Skeleton className="h-40 w-full rounded-xl" />
        )
      ) : (
        <ul className="divide-y rounded-xl border bg-card">
          {value.limitBack !== undefined && (
            <NotificationRow
              title="A limit is back"
              detail="When a limit you used up resets, without pressing its bell."
              checked={value.limitBack}
              onChange={(limitBack) => save({ ...value, limitBack })}
            />
          )}
          <NotificationRow
            title="A limit ran out"
            detail="When a 5-hour or weekly limit reaches 100%, with when it comes back."
            checked={value.ranOut}
            onChange={(ranOut) => save({ ...value, ranOut })}
          />
          <NotificationRow
            title="Running low"
            detail="Once per limit window, when it passes the level you choose."
            checked={value.lowAtPercent !== null}
            onChange={(on) => save({ ...value, lowAtPercent: on ? 90 : null })}
          >
            {value.lowAtPercent !== null && (
              <ToggleGroup
                type="single"
                variant="outline"
                spacing={0}
                value={String(value.lowAtPercent)}
                onValueChange={(v) => v && save({ ...value, lowAtPercent: Number(v) })}
                aria-label="Warn at"
                className="mt-3 w-full sm:w-fit"
              >
                {LOW_LEVELS.map((level) => (
                  <ToggleGroupItem
                    key={level}
                    value={String(level)}
                    className="num flex-1 px-4 data-[state=on]:bg-accent data-[state=on]:font-semibold sm:flex-none"
                  >
                    {level}%
                  </ToggleGroupItem>
                ))}
              </ToggleGroup>
            )}
          </NotificationRow>
        </ul>
      )}
    </Section>
  )
}

function NotificationRow({
  title,
  detail,
  checked,
  onChange,
  children,
}: {
  title: string
  detail: string
  checked: boolean
  onChange: (checked: boolean) => void
  children?: ReactNode
}) {
  const id = useId()
  return (
    <li className="px-4 py-3.5">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <label htmlFor={id} className="text-sm font-medium">
            {title}
          </label>
          <p id={`${id}-detail`} className="text-sm text-muted-foreground">
            {detail}
          </p>
        </div>
        <Switch id={id} checked={checked} onCheckedChange={onChange} aria-describedby={`${id}-detail`} className="mt-0.5" />
      </div>
      {children}
    </li>
  )
}

/** The three theme choices, in the order both the Settings toggle and the top-bar menu list them. */
export const THEMES: { value: ThemePreference; label: string; icon: typeof Sun }[] = [
  { value: 'light', label: 'Light', icon: Sun },
  { value: 'dark', label: 'Dark', icon: Moon },
  { value: 'system', label: 'System', icon: Monitor },
]

export function AppearanceSection() {
  const { preference, setPreference } = useTheme()
  return (
    <Section id="appearance-heading" title="Appearance" description="System follows your device and changes with it.">
      <ToggleGroup
        type="single"
        variant="outline"
        spacing={0}
        value={preference}
        onValueChange={(v) => v && setPreference(v as ThemePreference)}
        aria-label="Theme"
        className="w-full sm:w-fit"
      >
        {THEMES.map(({ value, label, icon: Icon }) => (
          <ToggleGroupItem
            key={value}
            value={value}
            className="flex-1 gap-2 px-4 data-[state=on]:bg-accent data-[state=on]:font-semibold sm:flex-none"
          >
            <Icon aria-hidden="true" />
            {label}
            {preference === value && <Check aria-hidden="true" className="size-3.5" />}
          </ToggleGroupItem>
        ))}
      </ToggleGroup>
    </Section>
  )
}
