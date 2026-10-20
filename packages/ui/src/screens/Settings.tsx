import { CircleCheck, CircleOff, Eye, EyeOff, Lock, Monitor, Smartphone, Tablet } from 'lucide-react'
import { useId, useState, type FormEvent, type ReactNode } from 'react'
import { toast } from 'sonner'
import { ProviderMark, SourceBadge, StatusBadge } from '@/components/limit/atoms'
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
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useSnapshotSource, useSourceQuery } from '@/data/SnapshotSource'
import type { AdapterInfo, HookStatus, PairedDevice } from '@/data/types'
import { formatFull, parseTime } from '@/lib/snapshot-view'
import { cn } from '@/lib/utils'
import { useAge } from '@/state/now'
import { Link } from '@/state/router'
import { PhoneSettingsScreen } from './PhoneSettings'

export function SettingsScreen() {
  const source = useSnapshotSource()
  // On a phone the keys and the Claude hook live on the PC: this screen shows the PC and Android permissions instead.
  if (source.phone) return <PhoneSettingsScreen />
  return (
    <div className="grid grid-cols-1 gap-8 xl:grid-cols-2 xl:items-start">
      <div className="flex flex-col gap-8">
        <NotificationsSection />
        <AdaptersSection />
      </div>
      <div className="flex flex-col gap-8">
        <HookSection />
        <DevicesSection />
        <AppearanceSection />
      </div>
    </div>
  )
}

/* ------------------------------------------------------------------ Adapters */

function AdaptersSection() {
  const adapters = useSourceQuery((s) => s.getAdapters())
  return (
    <Section
      id="adapters-heading"
      title="Agents and sources"
      description="Choose which agents appear on the dashboard. Each one is read on this device through its own official route: a terminal hook, its own CLI, or its API with your key. A source that is switched off is not read at all."
    >
      <p className="flex items-start gap-2.5 rounded-lg bg-secondary p-3 text-sm text-secondary-foreground">
        <Lock aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
        <span>
          <strong className="font-semibold">Keys stay on this device.</strong> They are kept in the system credential
          store and are never synced to paired devices. A key is only ever sent to the provider it belongs to.
        </span>
      </p>
      {adapters.error && !adapters.data ? (
        <ErrorState title="Could not load adapters" detail={adapters.error.message} onRetry={adapters.reload} />
      ) : !adapters.data ? (
        <Skeleton className="h-64 w-full rounded-xl" />
      ) : (
        <ul className="divide-y rounded-xl border bg-card">
          {adapters.data.map((adapter) => (
            <AdapterRow key={adapter.id} adapter={adapter} />
          ))}
        </ul>
      )}
    </Section>
  )
}

function AdapterRow({ adapter }: { adapter: AdapterInfo }) {
  const source = useSnapshotSource()
  const polled = parseTime(adapter.lastPolledAt)
  const polledAge = useAge(polled)
  const switchId = useId()
  const on = adapter.enabled !== false
  const [busy, setBusy] = useState(false)

  async function toggle(next: boolean) {
    setBusy(true)
    try {
      await source.setSourceEnabled!(adapter.id, next)
    } catch (err) {
      toast.error(`Could not switch ${adapter.name} ${next ? 'on' : 'off'}`, {
        description: err instanceof Error ? err.message : undefined,
      })
    } finally {
      setBusy(false)
    }
  }

  return (
    <li className="flex flex-col gap-3 p-4">
      <div className="flex items-start gap-3">
        <ProviderMark provider={adapter.provider} className={cn('size-9 text-xs', !on && 'opacity-50')} />
        <div className="min-w-0 flex-1">
          <label htmlFor={switchId} className={cn('text-base font-medium', !on && 'text-muted-foreground')}>
            {adapter.name}
          </label>
          {adapter.setup && <p className="max-w-prose text-sm text-muted-foreground">{adapter.setup}</p>}
          {on && (
            <p className="num mt-0.5 text-sm text-muted-foreground">
              v{adapter.version}
              {polled !== undefined ? ` · polled ${polledAge}` : ' · not polled yet'}
            </p>
          )}
        </div>
        {source.setSourceEnabled && (
          <Switch
            id={switchId}
            checked={on}
            disabled={busy}
            onCheckedChange={toggle}
            aria-label={`Show ${adapter.name} on the dashboard`}
            className="mt-1"
          />
        )}
      </div>
      {on && (
        <>
          <div className="flex flex-wrap items-center gap-2 pl-12">
            <SourceBadge kind={adapter.kind} />
            <StatusBadge status={adapter.status} />
          </div>
          {adapter.note && (
            <p className={cn('pl-12 text-sm', adapter.status === 'ok' ? 'text-muted-foreground' : 'text-sand-ink')}>
              {adapter.note}
            </p>
          )}
          {adapter.credentials !== 'none' && (
            <div className="pl-12">
              <KeyForm adapter={adapter} />
            </div>
          )}
          {adapter.hookAgent && source.agentHooks && (
            <div className="pl-12">
              <AgentHookControl agent={adapter.hookAgent} name={adapter.name} />
            </div>
          )}
          {adapter.credentials === 'github-token' && source.setCopilotAllowance && (
            <div className="pl-12">
              <AllowanceForm />
            </div>
          )}
        </>
      )}
    </li>
  )
}

/**
 * Install or remove the status line hook of a terminal agent (Antigravity CLI). Like the Claude hook,
 * installing edits the agent's own settings file after a backup and keeps any status line it had.
 */
function AgentHookControl({ agent, name }: { agent: string; name: string }) {
  const source = useSnapshotSource()
  const hooks = source.agentHooks!
  const status = useSourceQuery(() => hooks.status(agent), [agent])
  const [busy, setBusy] = useState(false)
  const [confirm, setConfirm] = useState(false)
  const installed = status.data?.installed === true
  const lastData = parseTime(status.data?.lastDataAt)
  const lastAge = useAge(lastData)

  async function run(kind: 'install' | 'uninstall') {
    setBusy(true)
    try {
      await hooks[kind](agent)
      setConfirm(false)
      status.reload()
      toast.success(kind === 'install' ? `Hook installed in ${name}` : `Hook removed from ${name}`)
    } catch (err) {
      toast.error(kind === 'install' ? 'Could not install the hook' : 'Could not remove the hook', {
        description: err instanceof Error ? err.message : undefined,
      })
    } finally {
      setBusy(false)
    }
  }

  if (!status.data) {
    return status.error ? (
      <p className="text-sm text-sand-ink">The hook status could not be read. {status.error.message}</p>
    ) : (
      <Skeleton className="h-10 w-48" />
    )
  }
  return (
    <div className="flex flex-col gap-2">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
        <span className={cn('inline-flex items-center gap-1.5 text-sm', installed ? 'text-tide-ink' : 'text-muted-foreground')}>
          {installed ? <CircleCheck aria-hidden="true" className="size-4" /> : <CircleOff aria-hidden="true" className="size-4" />}
          {installed ? 'Hook installed' : 'Hook not installed'}
          {installed && lastData !== undefined && <span className="text-muted-foreground">· last reading {lastAge}</span>}
        </span>
        <Button type="button" size="sm" variant={installed ? 'outline' : 'default'} disabled={busy} onClick={() => (installed ? run('uninstall') : setConfirm(true))}>
          {installed ? 'Uninstall hook' : 'Install hook'}
        </Button>
      </div>
      <p className="num text-xs break-all text-muted-foreground">{status.data.settingsPath}</p>
      <Dialog open={confirm} onOpenChange={setConfirm}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Install the hook in {name}?</DialogTitle>
            <DialogDescription>
              Cooldown adds a status line command to {status.data.settingsPath}. If you already have a status line, it is
              backed up and still runs. Cooldown only keeps the quota figures {name} passes to it, nothing else. Uninstalling
              puts everything back.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex-col-reverse gap-2 sm:flex-row sm:justify-end">
            <DialogClose asChild>
              <Button type="button" variant="outline">
                Cancel
              </Button>
            </DialogClose>
            <Button type="button" disabled={busy} onClick={() => run('install')}>
              Install hook
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

/**
 * The Copilot plan's monthly AI-credit allowance. GitHub's billing API reports spend only, and the
 * published allowances differ by plan and change, so the user types theirs. Empty shows spend only.
 */
function AllowanceForm() {
  const source = useSnapshotSource()
  const id = useId()
  const helpId = useId()
  const current = useSourceQuery((s) => s.getCopilotAllowance?.() ?? Promise.resolve(null))
  const [value, setValue] = useState('')
  const [busy, setBusy] = useState(false)
  const saved = current.data ?? null

  async function onSubmit(e: FormEvent) {
    e.preventDefault()
    const text = value.trim()
    const usd = text === '' ? null : Number(text)
    if (usd !== null && !(Number.isFinite(usd) && usd > 0)) {
      toast.error('Enter the allowance in dollars, for example 10 or 39.')
      return
    }
    setBusy(true)
    try {
      await source.setCopilotAllowance!(usd)
      setValue('')
      toast.success(usd === null ? 'Allowance cleared: spend only' : `Allowance set to $${usd}`)
    } catch (err) {
      toast.error('Could not save the allowance', { description: err instanceof Error ? err.message : undefined })
    } finally {
      setBusy(false)
    }
  }

  return (
    <form onSubmit={onSubmit} className="flex flex-col gap-1.5">
      <Label htmlFor={id}>
        Monthly AI credit allowance <span className="font-normal text-muted-foreground">(USD, optional)</span>
      </Label>
      <div className="flex gap-2">
        <Input
          id={id}
          inputMode="decimal"
          value={value}
          onChange={(e) => setValue(e.target.value)}
          placeholder={saved === null ? 'For example 10 for Pro, 39 for Pro+' : `Now $${saved}. Type a new amount, or clear it`}
          aria-describedby={helpId}
          className="min-w-0 flex-1"
        />
        <Button type="submit" variant="outline" disabled={busy}>
          {value.trim() === '' && saved !== null ? 'Clear' : 'Save'}
        </Button>
      </div>
      <p id={helpId} className="max-w-prose text-sm text-muted-foreground">
        GitHub reports what you spent, not what your plan includes. With an allowance, Cooldown shows a percentage and can
        warn you before it runs out.
      </p>
    </form>
  )
}

function KeyForm({ adapter }: { adapter: AdapterInfo }) {
  const source = useSnapshotSource()
  const keyId = useId()
  const teamId = useId()
  const teamHelpId = useId()
  const noteId = useId()
  const [key, setKey] = useState('')
  const [team, setTeam] = useState('')
  const [reveal, setReveal] = useState(false)
  const [busy, setBusy] = useState(false)
  const [confirmRemove, setConfirmRemove] = useState(false)
  // A management key (xAI) can take a team ID, but only needs one when it covers a whole organization.
  const hasTeamField = adapter.credentials === 'management-key'
  const isGithub = adapter.credentials === 'github-token'
  const label = hasTeamField ? 'Management key' : isGithub ? 'GitHub token' : 'API key'

  async function onSubmit(e: FormEvent) {
    e.preventDefault()
    if (!key.trim()) return
    setBusy(true)
    try {
      await source.saveApiKey(adapter.id, key, hasTeamField && team.trim() ? { teamId: team.trim() } : undefined)
      setKey('')
      setTeam('')
      setReveal(false)
      toast.success(`${adapter.name} key saved on this device`)
    } catch (err) {
      toast.error('Could not save the key', { description: err instanceof Error ? err.message : undefined })
    } finally {
      setBusy(false)
    }
  }

  async function onRemove() {
    try {
      await source.removeApiKey(adapter.id)
      setConfirmRemove(false)
      toast(`${adapter.name} key removed from this device`)
    } catch (err) {
      toast.error('Could not remove the key', { description: err instanceof Error ? err.message : undefined })
    }
  }

  return (
    <form onSubmit={onSubmit} className="flex flex-col gap-3" aria-describedby={noteId}>
      {hasTeamField && (
        <div className="flex flex-col gap-1.5">
          <Label htmlFor={teamId}>
            Team ID <span className="font-normal text-muted-foreground">(optional)</span>
          </Label>
          <Input
            id={teamId}
            value={team}
            onChange={(e) => setTeam(e.target.value)}
            autoComplete="off"
            spellCheck={false}
            name="xai-team-id"
            placeholder="Leave empty unless the note above asks for it…"
            aria-describedby={teamHelpId}
          />
          <p id={teamHelpId} className="max-w-prose text-sm text-muted-foreground">
            Cooldown finds your team from the key. Fill this in only for a key that covers your whole organization, where xAI
            cannot say which team to read. Saving a key replaces the saved team ID, so type it again whenever you paste a new key.
          </p>
        </div>
      )}
      <div className="flex flex-col gap-1.5">
        <Label htmlFor={keyId}>{label}</Label>
        {isGithub && (
          <p className="max-w-prose text-sm text-muted-foreground">
            Create a fine-grained token at github.com → Settings → Developer settings → Fine-grained tokens, with only{' '}
            <strong className="font-medium text-foreground">Account permissions → Plan: Read-only</strong>. It can read your
            billing usage and nothing else.
          </p>
        )}
        <div className="flex gap-2">
          <div className="relative min-w-0 flex-1">
            <Input
              id={keyId}
              name={`${adapter.id}-key`}
              type={reveal ? 'text' : 'password'}
              value={key}
              onChange={(e) => setKey(e.target.value)}
              autoComplete="off"
              autoCorrect="off"
              autoCapitalize="off"
              spellCheck={false}
              placeholder={adapter.hasKey ? 'Paste to replace…' : 'Paste key…'}
              className="pr-12"
            />
            <Button
              type="button"
              variant="ghost"
              size="icon"
              onClick={() => setReveal((r) => !r)}
              aria-label={reveal ? `Hide ${label.toLowerCase()}` : `Show ${label.toLowerCase()}`}
              aria-pressed={reveal}
              className="absolute top-0 right-0"
            >
              {reveal ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
            </Button>
          </div>
          <Button type="submit" disabled={busy || !key.trim()}>
            Save key
          </Button>
        </div>
      </div>
      <p id={noteId} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm text-muted-foreground">
        {adapter.hasKey ? (
          <>
            <span className="inline-flex items-center gap-1.5 text-tide-ink">
              <CircleCheck aria-hidden="true" className="size-4" />A key is saved on this device
            </span>
            <Button type="button" variant="outline" size="sm" onClick={() => setConfirmRemove(true)}>
              Remove key
            </Button>
          </>
        ) : (
          <span className="inline-flex items-center gap-1.5">
            <CircleOff aria-hidden="true" className="size-4" />
            No key saved. This adapter is off until you add one.
          </span>
        )}
      </p>
      <Dialog open={confirmRemove} onOpenChange={setConfirmRemove}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Remove the {adapter.name} key?</DialogTitle>
            <DialogDescription>
              The key is deleted from this device’s credential store. The adapter stops reading {adapter.name} until you
              paste a key again. Your last good value stays visible on the overview.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex-col-reverse gap-2 sm:flex-row sm:justify-end">
            <DialogClose asChild>
              <Button type="button" variant="outline">Keep key</Button>
            </DialogClose>
            <Button type="button" variant="destructive" onClick={onRemove}>
              Remove key
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </form>
  )
}

/* ---------------------------------------------------------------------- Hook */

function HookSection() {
  const hook = useSourceQuery((s) => s.getHookStatus())
  return (
    <Section
      id="hook-heading"
      title="Claude Code statusline hook"
      description="The only way Cooldown reads Claude limits. It uses Claude Code's own statusline, never your Claude login."
    >
      {hook.error && !hook.data ? (
        <ErrorState title="Could not read the hook status" detail={hook.error.message} onRetry={hook.reload} />
      ) : !hook.data ? (
        <Skeleton className="h-48 w-full rounded-xl" />
      ) : (
        <HookBlock hook={hook.data} />
      )}
    </Section>
  )
}

function HookBlock({ hook }: { hook: HookStatus }) {
  const source = useSnapshotSource()
  const [open, setOpen] = useState<'install' | 'uninstall' | null>(null)
  const [busy, setBusy] = useState(false)
  const last = parseTime(hook.lastDataAt)
  const lastAge = useAge(last)

  async function run(kind: 'install' | 'uninstall') {
    setBusy(true)
    try {
      if (kind === 'install') await source.installHook()
      else await source.uninstallHook()
      toast.success(kind === 'install' ? 'Statusline hook installed' : 'Statusline hook removed')
      setOpen(null)
    } catch (err) {
      toast.error(kind === 'install' ? 'Could not install the hook' : 'Could not remove the hook', {
        description: err instanceof Error ? err.message : undefined,
      })
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex flex-col gap-4 rounded-xl border bg-card p-4 sm:p-5">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p
          className={cn(
            'inline-flex items-center gap-2 text-base font-semibold',
            hook.installed ? 'text-tide-ink' : 'text-muted-foreground',
          )}
        >
          {hook.installed ? <CircleCheck aria-hidden="true" className="size-5" /> : <CircleOff aria-hidden="true" className="size-5" />}
          {hook.installed ? 'Installed' : 'Not installed'}
        </p>
        {hook.installed ? (
          <Button variant="outline" onClick={() => setOpen('uninstall')}>
            Uninstall hook
          </Button>
        ) : (
          <Button onClick={() => setOpen('install')}>Install hook</Button>
        )}
      </div>

      <dl className="flex flex-col text-sm">
        <HookRow label="Settings file"><span translate="no">{hook.settingsPath}</span></HookRow>
        {hook.installed && (
          <>
            <HookRow label="Your statusline">
              {hook.chainsExisting ? `Backed up to ${hook.backupPath}, and still run by the hook.` : 'No existing statusline to keep.'}
            </HookRow>
            <HookRow label="Inbox file"><span translate="no">{hook.inboxPath}</span></HookRow>
            <HookRow label="Last reading">
              {last === undefined ? (
                'Nothing received yet. Start Claude Code and it will appear.'
              ) : (
                <>
                  <span className="num">{formatFull(last)}</span>
                  <span className="text-muted-foreground"> ({lastAge})</span>
                </>
              )}
            </HookRow>
          </>
        )}
      </dl>

      <p className="text-sm text-muted-foreground">
        Figures are only fresh while Claude Code is running. Installing backs up your current statusline and chains
        it; uninstalling puts it back.
      </p>

      <Dialog open={open !== null} onOpenChange={(o) => !o && setOpen(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{open === 'uninstall' ? 'Uninstall the statusline hook?' : 'Install the statusline hook?'}</DialogTitle>
            <DialogDescription>
              {open === 'uninstall'
                ? 'Cooldown restores your original statusline setting from the backup. Claude figures stop updating until you install the hook again.'
                : `Cooldown backs up the statusLine entry in ${hook.settingsPath}, then adds a command that writes your rate limits to a local file and still runs your original statusline. You can uninstall it at any time.`}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex-col-reverse gap-2 sm:flex-row sm:justify-end">
            <DialogClose asChild>
              <Button variant="outline">Cancel</Button>
            </DialogClose>
            <Button disabled={busy} onClick={() => open && run(open)}>
              {open === 'uninstall' ? 'Uninstall hook' : 'Install hook'}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

function HookRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="grid gap-0.5 border-t py-2.5 first:border-t-0 first:pt-0 sm:grid-cols-[8.5rem_minmax(0,1fr)] sm:gap-4">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 [overflow-wrap:anywhere]">{children}</dd>
    </div>
  )
}

/* ------------------------------------------------------------------- Devices */

function DevicesSection() {
  const devices = useSourceQuery((s) => s.getPairedDevices())
  return (
    <Section
      id="devices-heading"
      title="Paired devices"
      description="Phones and tablets that receive readings and bell times from this PC. Revoking deletes the key this PC holds for that device."
    >
      {devices.error && !devices.data ? (
        <ErrorState title="Could not load paired devices" detail={devices.error.message} onRetry={devices.reload} />
      ) : !devices.data ? (
        <Skeleton className="h-32 w-full rounded-xl" />
      ) : devices.data.length === 0 ? (
        <div className="flex flex-col items-start gap-3 rounded-xl border border-dashed bg-card p-5">
          <p className="text-sm text-muted-foreground">No devices are paired. Pair a phone to get your bells there too.</p>
          <Button asChild>
            <Link to={{ name: 'pairing' }}>Pair a device</Link>
          </Button>
        </div>
      ) : (
        <>
          <ul className="divide-y rounded-xl border bg-card">
            {devices.data.map((d) => (
              <DeviceRow key={d.id} device={d} />
            ))}
          </ul>
          <div>
            <Button asChild variant="outline">
              <Link to={{ name: 'pairing' }}>Pair another device</Link>
            </Button>
          </div>
        </>
      )}
    </Section>
  )
}

function DeviceRow({ device }: { device: PairedDevice }) {
  const source = useSnapshotSource()
  const [confirm, setConfirm] = useState(false)
  const [busy, setBusy] = useState(false)
  const seen = parseTime(device.lastSeenAt)
  const seenAge = useAge(seen)
  const Icon = device.name.toLowerCase().includes('tab') ? Tablet : device.platform === 'windows' ? Monitor : Smartphone

  async function revoke() {
    setBusy(true)
    try {
      await source.revokeDevice(device.id)
      toast(`${device.name} revoked`, { description: 'It no longer receives readings or bells from this PC.' })
      setConfirm(false)
    } catch (err) {
      toast.error('Could not revoke the device', { description: err instanceof Error ? err.message : undefined })
    } finally {
      setBusy(false)
    }
  }

  return (
    <li className="flex items-center gap-3 p-4">
      <span aria-hidden="true" className="grid size-10 shrink-0 place-items-center rounded-lg bg-secondary text-secondary-foreground">
        <Icon className="size-5" />
      </span>
      <div className="min-w-0 flex-1">
        <p className="truncate text-base font-medium">{device.name}</p>
        <p className="num text-sm text-muted-foreground">
          {seen === undefined ? 'Never connected' : `Last seen ${seenAge}`}
        </p>
      </div>
      <Button variant="outline" onClick={() => setConfirm(true)} aria-label={`Revoke ${device.name}`}>
        Revoke
      </Button>
      <Dialog open={confirm} onOpenChange={setConfirm}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Revoke {device.name}?</DialogTitle>
            <DialogDescription>
              Its key is deleted on this PC. It stops receiving readings and bells until you pair it again.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter className="flex-col-reverse gap-2 sm:flex-row sm:justify-end">
            <DialogClose asChild>
              <Button variant="outline">Keep device</Button>
            </DialogClose>
            <Button variant="destructive" disabled={busy} onClick={revoke}>
              Revoke {device.name}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </li>
  )
}
