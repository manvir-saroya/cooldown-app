import { Check, CircleCheck, Copy, EyeOff, RefreshCw, Smartphone, TriangleAlert, Wifi } from 'lucide-react'
import { QRCodeSVG } from 'qrcode.react'
import { useCallback, useEffect, useRef, useState } from 'react'
import { toast } from 'sonner'
import { Countdown } from '@/components/limit/atoms'
import { ErrorState } from '@/components/ScreenStates'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useSnapshotSource, useSourceQuery } from '@/data/SnapshotSource'
import type { PairingPayload, PairingState } from '@/data/types'
import { cn } from '@/lib/utils'
import { useNowSelector } from '@/state/now'
import { groupCode, PhonePairingScreen } from './PhonePairing'

export function PairingScreen() {
  const source = useSnapshotSource()
  // On a phone pairing runs the other way round: it scans the PC's code instead of showing one.
  return source.phone ? <PhonePairingScreen phone={source.phone} /> : <PcPairingScreen />
}

function PcPairingScreen() {
  const source = useSnapshotSource()
  const [payload, setPayload] = useState<PairingPayload>()
  const [error, setError] = useState<Error>()
  const [copied, setCopied] = useState(false)
  const [answering, setAnswering] = useState(false)
  // True after the app window was closed to the tray: the code was dropped and a new one is needed.
  const [dropped, setDropped] = useState(false)
  const control = source.pairing
  // Counts requests, so a code that arrives after the window closed is thrown away unseen.
  const request = useRef(0)
  const copiedTimer = useRef<ReturnType<typeof setTimeout>>(undefined)
  const confirmRef = useRef<HTMLElement>(null)
  const pairing = useSourceQuery<PairingState>((s) => (s.pairing ? s.pairing.getState() : Promise.resolve({ phase: 'idle' })))
  const state = pairing.data ?? { phase: 'idle' }
  const expiresAt = payload ? Date.parse(payload.expiresAt) : undefined
  // Flips once, when the code runs out. The countdown beside it ticks on its own.
  const expired = useNowSelector((now) => expiresAt !== undefined && expiresAt <= now)

  const load = useCallback(() => {
    const mine = ++request.current
    setError(undefined)
    setDropped(false)
    source.createPairingPayload().then(
      (made) => {
        if (request.current === mine) setPayload(made)
      },
      (e: unknown) => {
        if (request.current === mine) setError(e instanceof Error ? e : new Error(String(e)))
      },
    )
  }, [source])

  useEffect(load, [load])

  // Closing the window leaves the app running in the tray with this page still loaded. The QR code
  // and the typed code are one-time secrets, so they go when the window does (the backend closes
  // the pairing window too). When the window comes back, a fresh code is made.
  useEffect(() => {
    const stop = control?.onWindowHidden?.(() => {
      request.current += 1
      setPayload(undefined)
      setDropped(true)
    })
    return stop
  }, [control])

  useEffect(() => {
    if (!dropped) return
    const back = () => {
      if (document.visibilityState !== 'hidden') load()
    }
    window.addEventListener('focus', back)
    document.addEventListener('visibilitychange', back)
    return () => {
      window.removeEventListener('focus', back)
      document.removeEventListener('visibilitychange', back)
    }
  }, [dropped, load])

  useEffect(() => () => clearTimeout(copiedTimer.current), [])

  // The phone asks for a decision: bring the question into view and tell assistive tech, which on a
  // narrow screen would otherwise sit below the QR code unseen.
  const confirming = state.phase === 'confirm'
  useEffect(() => {
    if (!confirming) return
    confirmRef.current?.focus({ preventScroll: true })
    confirmRef.current?.scrollIntoView({ block: 'nearest' })
  }, [confirming])

  // Leaving the screen hides the code, so its one-time secret stops working.
  useEffect(() => () => void control?.cancel().catch(() => {}), [control])

  async function answer(accept: boolean) {
    if (!control) return
    setAnswering(true)
    try {
      await control.confirm(accept)
    } catch (e) {
      toast.error('Could not answer the phone', { description: e instanceof Error ? e.message : String(e) })
    } finally {
      setAnswering(false)
      pairing.reload()
    }
  }

  if (error && !payload) return <ErrorState title="Could not create a pairing code" detail={error.message} onRetry={load} />


  async function copy() {
    if (!payload) return
    try {
      await navigator.clipboard.writeText(payload.manualCode)
      setCopied(true)
      toast.success('Code copied')
      clearTimeout(copiedTimer.current)
      copiedTimer.current = setTimeout(() => setCopied(false), 2000)
    } catch {
      toast.error('Could not copy', { description: 'Select the code and copy it by hand.' })
    }
  }

  return (
    <div className="grid grid-cols-1 gap-6 lg:grid-cols-[22rem_minmax(0,1fr)] lg:items-start">
      <section aria-labelledby="qr-heading" className="flex flex-col items-center gap-4 rounded-xl border bg-card p-5">
        <h2 id="qr-heading" className="self-start text-base font-semibold">
          Scan with your phone
        </h2>
        {/* A QR code always needs a white quiet zone and dark modules, in both themes. */}
        <div className={cn('rounded-lg bg-white p-4 transition-opacity', expired && 'opacity-30')}>
          {payload ? (
            <QRCodeSVG
              value={payload.uri}
              size={208}
              level="M"
              bgColor="#ffffff"
              fgColor="#0b1b2e"
              title="Pairing code for this PC"
            />
          ) : dropped ? (
            <div className="grid size-52 place-items-center gap-2 text-center text-sm text-muted-foreground">
              <EyeOff aria-hidden="true" className="size-6" />
              <p>The code was hidden when the window closed.</p>
            </div>
          ) : (
            <Skeleton className="size-52" />
          )}
        </div>
        {/* The ticking countdown stays out of a live region; only the moment it runs out is announced. */}
        <p className="num text-sm text-muted-foreground">
          {payload && expiresAt !== undefined ? (
            expired ? (
              'This code has expired.'
            ) : (
              <>
                Expires in <Countdown target={expiresAt} className="font-medium text-foreground" />
              </>
            )
          ) : dropped ? (
            'No code is showing.'
          ) : (
            'Making a code…'
          )}
        </p>
        <span role="status" className="sr-only">
          {expired ? 'The pairing code has expired. Get a new code.' : ''}
        </span>
        <Button variant={expired || dropped ? 'default' : 'outline'} onClick={load} className="w-full">
          <RefreshCw aria-hidden="true" />
          Get a new code
        </Button>
        {payload?.placeholder && (
          <p className="text-center text-sm text-muted-foreground">
            Sample code. A real one carries this PC’s address, its public key and a one-time secret.
          </p>
        )}
      </section>

      <div className="flex flex-col gap-6">
        {state.phase === 'confirm' && (
          <section
            ref={confirmRef}
            tabIndex={-1}
            aria-labelledby="confirm-heading"
            role="alertdialog"
            className="flex flex-col gap-4 rounded-xl border-2 border-primary bg-card p-4 outline-none sm:p-5"
          >
            <div className="flex items-start gap-3">
              <Smartphone aria-hidden="true" className="mt-0.5 size-5 shrink-0" />
              <div className="space-y-1">
                <h2 id="confirm-heading" className="text-base font-semibold">
                  Pair “{state.name}”?
                </h2>
                <p className="text-sm text-muted-foreground">
                  Only confirm if your phone shows exactly this code. If it does not, someone else may be trying to pair.
                </p>
              </div>
            </div>
            <p translate="no" className="num self-start rounded-lg bg-secondary px-4 py-2.5 text-3xl font-semibold tracking-wider">
              {groupCode(state.code)}
            </p>
            <div className="flex flex-wrap gap-3">
              <Button onClick={() => void answer(true)} disabled={answering}>
                It matches, pair
              </Button>
              <Button variant="outline" onClick={() => void answer(false)} disabled={answering}>
                It does not match
              </Button>
            </div>
          </section>
        )}
        {state.phase === 'paired' && (
          <section aria-live="polite" className="flex items-start gap-3 rounded-xl bg-tide-soft p-4 text-sm sm:p-5">
            <CircleCheck aria-hidden="true" className="mt-0.5 size-5 shrink-0 text-tide-ink" />
            <p>
              <span className="font-semibold">{state.name}</span> is paired. It gets your limits and bells whenever it is on
              the same network as this PC.
            </p>
          </section>
        )}
        {state.phase === 'failed' && (
          <section aria-live="polite" className="flex items-start gap-3 rounded-xl border bg-card p-4 text-sm sm:p-5">
            <TriangleAlert aria-hidden="true" className="mt-0.5 size-5 shrink-0 text-sand-ink" />
            <p>{state.reason}</p>
          </section>
        )}
        <section aria-labelledby="steps-heading">
          <h2 id="steps-heading" className="text-base font-semibold">
            How to pair
          </h2>
          <ol className="mt-3 flex flex-col gap-3 text-sm">
            {[
              'Open Cooldown on your phone and choose Pair.',
              'Point the camera at the code on this screen.',
              control ? (
                'Your phone shows a six-digit code. This screen shows one too: confirm here only if they match.'
              ) : (
                <>
                  Check that both screens show{' '}
                  <span className="num rounded-md bg-secondary px-1.5 py-0.5 font-semibold whitespace-nowrap">{payload?.fingerprint ?? '…'}</span>, then
                  confirm on the phone.
                </>
              ),
            ].map((step, i) => (
              <li key={i} className="flex items-start gap-3">
                <span aria-hidden="true" className="num grid size-6 shrink-0 place-items-center rounded-full bg-secondary text-xs font-semibold">
                  {i + 1}
                </span>
                <span className="pt-0.5">{step}</span>
              </li>
            ))}
          </ol>
        </section>

        {(!payload || payload.manualCode !== '') && (
          <section aria-labelledby="manual-heading" className="rounded-xl border bg-card p-4 sm:p-5">
            <h2 id="manual-heading" className="text-base font-semibold">
              Cannot scan? Copy the code
            </h2>
            <p className="mt-1 max-w-prose text-sm text-muted-foreground">
              On the phone, choose “Paste code instead” and paste this. It is the same one-time code as the QR code, so it
              stops working when the QR code does. Do not share it.
            </p>
            <p
              translate="no"
              className="mt-3 rounded-lg bg-secondary px-3 py-2.5 font-mono text-sm break-all select-all"
            >
              {payload?.manualCode ?? '—'}
            </p>
            <div className="mt-3 flex flex-wrap items-center gap-3">
              <Button variant="outline" onClick={copy} disabled={!payload}>
                {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                {copied ? 'Copied' : 'Copy code'}
              </Button>
              {payload && (
                <p className="num text-sm text-muted-foreground">
                  Address {payload.host}:{payload.port}
                </p>
              )}
            </div>
          </section>
        )}

        <section aria-labelledby="local-heading" className="flex items-start gap-3 rounded-xl bg-tide-soft p-4 text-sm sm:p-5">
          <Wifi aria-hidden="true" className="mt-0.5 size-5 shrink-0 text-tide-ink" />
          <div className="space-y-1">
            <h2 id="local-heading" className="text-base font-semibold">
              Local network only
            </h2>
            <p className="max-w-prose text-muted-foreground">
              Both devices must be on the same private network, such as your home Wi-Fi. Pairing and syncing go
              straight between them. There is no Cooldown server in the middle, and no account. Only readings and
              bell times are sent: never an API key, token or cookie.
            </p>
          </div>
        </section>
      </div>
    </div>
  )
}
