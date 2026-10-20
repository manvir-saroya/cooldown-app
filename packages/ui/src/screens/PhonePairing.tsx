/**
 * Pairing as seen from the phone: scan the code the PC shows, compare six digits, and wait for the
 * PC to confirm. The PC always has the last word, so this screen never says "paired" before it says so.
 */
import { CircleCheck, ClipboardPaste, LoaderCircle, ScanLine, TriangleAlert, Wifi } from 'lucide-react'
import { useEffect, useId, useRef, useState } from 'react'
import { PermissionNudge } from '@/components/phone/permissions'
import { Button } from '@/components/ui/button'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import type { PhoneControl } from '@/data/SnapshotSource'
import { errorMessage, isCancelled, usePairedPcs } from '@/hooks/use-phone'
import { Link } from '@/state/router'

type Step =
  | { name: 'idle' }
  | { name: 'scanning' }
  | { name: 'connecting' }
  | { name: 'confirm'; code: string; pcName: string }
  | { name: 'paired'; pcName: string }
  | { name: 'failed'; reason: string }

/** "482193" reads as "482 193". Anything that is not six digits is shown as given. */
export function groupCode(code: string): string {
  return /^\d{6}$/.test(code) ? `${code.slice(0, 3)} ${code.slice(3)}` : code
}

export function PhonePairingScreen({ phone }: { phone: PhoneControl }) {
  const [step, setStep] = useState<Step>({ name: 'idle' })
  const [pasting, setPasting] = useState(false)
  const [text, setText] = useState('')
  const pcs = usePairedPcs()
  const textId = useId()
  const run = useRef(0)

  // Leaving the screen abandons a pairing in progress: its late answers must not touch the screen.
  useEffect(
    () => () => {
      run.current += 1
    },
    [],
  )

  async function connect(id: number, qrText: string) {
    const live = () => run.current === id
    setStep({ name: 'connecting' })
    try {
      const { code, pcName } = await phone.beginPairing(qrText.trim())
      if (!live()) return
      setStep({ name: 'confirm', code, pcName })
      await phone.finishPairing()
      if (!live()) return
      setStep({ name: 'paired', pcName })
      setText('')
      setPasting(false)
      pcs.reload()
    } catch (err) {
      if (live()) setStep({ name: 'failed', reason: errorMessage(err, 'Pairing did not finish. Try again.') })
    }
  }

  async function scan() {
    const id = ++run.current
    setStep({ name: 'scanning' })
    let qrText: string
    try {
      qrText = await phone.scanQr()
    } catch (err) {
      if (run.current !== id) return
      setStep(isCancelled(err) ? { name: 'idle' } : { name: 'failed', reason: errorMessage(err, 'The camera could not read a code.') })
      return
    }
    if (run.current === id) await connect(id, qrText)
  }

  function pasteAndPair() {
    if (!text.trim()) return
    void connect(++run.current, text)
  }

  function reset() {
    run.current += 1
    setStep({ name: 'idle' })
  }

  const busy = step.name === 'scanning' || step.name === 'connecting'
  const paired = pcs.data ?? []

  return (
    <div className="flex max-w-xl flex-col gap-6">
      {step.name === 'idle' && paired.length > 0 && (
        <p className="flex items-start gap-3 rounded-xl bg-tide-soft p-4 text-sm">
          <CircleCheck aria-hidden="true" className="mt-0.5 size-5 shrink-0 text-tide-ink" />
          <span>
            Already paired with <span className="font-semibold" translate="no">{paired.map((p) => p.name).join(', ')}</span>. You only need
            this to pair a different PC.
          </span>
        </p>
      )}

      {(step.name === 'idle' || step.name === 'scanning') && (
        <section aria-labelledby="scan-heading" className="flex flex-col gap-4 rounded-xl border bg-card p-4 sm:p-5">
          <h2 id="scan-heading" className="text-base font-semibold">
            Scan the code on your PC
          </h2>
          <ol className="flex flex-col gap-3 text-sm">
            {[
              'On your PC, open Cooldown and choose Pair a device.',
              'Point this phone’s camera at the code on the PC screen.',
              'This phone shows six digits. Confirm on the PC only if they match.',
            ].map((line, i) => (
              <li key={i} className="flex items-start gap-3">
                <span aria-hidden="true" className="num grid size-6 shrink-0 place-items-center rounded-full bg-secondary text-xs font-semibold">
                  {i + 1}
                </span>
                <span className="pt-0.5">{line}</span>
              </li>
            ))}
          </ol>
          <Button size="lg" className="w-full" onClick={() => void scan()} disabled={busy}>
            {step.name === 'scanning' ? <LoaderCircle aria-hidden="true" className="animate-spin" /> : <ScanLine aria-hidden="true" />}
            {step.name === 'scanning' ? 'Opening the camera…' : 'Scan code'}
          </Button>
          <Button
            variant="outline"
            className="w-full"
            onClick={() => setPasting((p) => !p)}
            aria-expanded={pasting}
            aria-controls={textId}
            disabled={busy}
          >
            <ClipboardPaste aria-hidden="true" />
            Paste code instead
          </Button>
          {pasting && (
            <form
              id={textId}
              className="flex flex-col gap-3"
              onSubmit={(e) => {
                e.preventDefault()
                pasteAndPair()
              }}
            >
              <div className="flex flex-col gap-1.5">
                <Label htmlFor={`${textId}-text`}>Code from your PC</Label>
                <Textarea
                  id={`${textId}-text`}
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                  rows={3}
                  autoComplete="off"
                  autoCorrect="off"
                  autoCapitalize="off"
                  spellCheck={false}
                  translate="no"
                  placeholder="Paste the code here…"
                  className="font-mono text-sm break-all"
                />
              </div>
              <Button type="submit" disabled={!text.trim()}>
                Pair
              </Button>
            </form>
          )}
        </section>
      )}

      {step.name === 'connecting' && (
        <section role="status" className="flex items-center gap-3 rounded-xl border bg-card p-4 sm:p-5">
          <LoaderCircle aria-hidden="true" className="size-5 shrink-0 animate-spin" />
          <p className="flex-1 text-base font-medium">Connecting to your PC…</p>
          <Button variant="outline" onClick={reset}>
            Cancel
          </Button>
        </section>
      )}

      {step.name === 'confirm' && (
        <section
          aria-labelledby="confirm-heading"
          aria-live="polite"
          className="flex flex-col gap-4 rounded-xl border-2 border-primary bg-card p-4 sm:p-5"
        >
          <div className="space-y-1">
            <h2 id="confirm-heading" className="text-lg font-semibold">
              Confirm it on your PC
            </h2>
            <p className="text-sm text-muted-foreground">
              <span translate="no" className="font-medium text-foreground">
                {step.pcName}
              </span>{' '}
              should show the same six digits. On the PC choose “It matches, pair”. If they differ, choose “It does not match”.
            </p>
          </div>
          <p
            translate="no"
            aria-label={`Code ${step.code.split('').join(' ')}`}
            className="num self-start rounded-lg bg-secondary px-4 py-3 text-5xl font-semibold tracking-wider select-all"
          >
            {groupCode(step.code)}
          </p>
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <LoaderCircle aria-hidden="true" className="size-4 animate-spin" />
            Waiting for your PC…
          </p>
          <Button variant="outline" onClick={reset}>
            Cancel
          </Button>
        </section>
      )}

      {step.name === 'paired' && (
        <>
          <section aria-live="polite" className="flex flex-col gap-4 rounded-xl bg-tide-soft p-4 sm:p-5">
            <div className="flex items-start gap-3">
              <CircleCheck aria-hidden="true" className="mt-0.5 size-5 shrink-0 text-tide-ink" />
              <div className="space-y-1">
                <h2 className="text-base font-semibold">
                  Paired with <span translate="no">{step.pcName}</span>
                </h2>
                <p className="text-sm">
                  Your limits and bells arrive whenever this phone is on the same network as the PC.
                </p>
              </div>
            </div>
            <Button asChild size="lg">
              <Link to={{ name: 'overview' }}>See your limits</Link>
            </Button>
          </section>
          <PermissionNudge />
        </>
      )}

      {step.name === 'failed' && (
        <section role="alert" className="flex flex-col gap-4 rounded-xl border bg-card p-4 sm:p-5">
          <div className="flex items-start gap-3">
            <TriangleAlert aria-hidden="true" className="mt-0.5 size-5 shrink-0 text-sand-ink" />
            <div className="space-y-1">
              <h2 className="text-base font-semibold">Could not pair</h2>
              <p className="text-sm text-muted-foreground">{step.reason}</p>
              <p className="text-sm text-muted-foreground">
                If the code has expired, choose “Get a new code” on your PC and scan that one.
              </p>
            </div>
          </div>
          <Button onClick={reset}>Try again</Button>
        </section>
      )}

      <section aria-labelledby="local-heading" className="flex items-start gap-3 rounded-xl bg-tide-soft p-4 text-sm sm:p-5">
        <Wifi aria-hidden="true" className="mt-0.5 size-5 shrink-0 text-tide-ink" />
        <div className="space-y-1">
          <h2 id="local-heading" className="text-base font-semibold">
            Local network only
          </h2>
          <p className="max-w-prose text-muted-foreground">
            Your phone and PC must be on the same private network, such as your home Wi-Fi. They talk straight to each
            other: there is no Cooldown server in the middle and no account. Only readings and bell times are sent,
            never an API key.
          </p>
        </div>
      </section>
    </div>
  )
}
