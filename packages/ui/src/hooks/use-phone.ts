/**
 * Phone-mode helpers. Everything here is inert on a source without `phone`, so PC mode never calls it.
 */
import { useCallback, useEffect, useRef, useState } from 'react'
import { useSnapshotSource, useSourceQuery, type Query } from '@/data/SnapshotSource'
import type { PairedPc, PhonePermissions } from '@/data/types'

export function errorMessage(err: unknown, fallback: string): string {
  const text = err instanceof Error ? err.message : typeof err === 'string' ? err : ''
  return text.trim() || fallback
}

/** True when the user backed out of the camera on purpose. Not an error to show. */
export function isCancelled(err: unknown): boolean {
  return /cancel/i.test(errorMessage(err, ''))
}

/** The PCs this phone is paired with. Refetches whenever the source says something changed. */
export function usePairedPcs(): Query<PairedPc[]> {
  return useSourceQuery((s) => (s.phone ? s.phone.pairedPcs() : Promise.resolve([])))
}

export type PermissionKey = keyof PhonePermissions

export const PERMISSION_KEYS: PermissionKey[] = ['exactAlarms', 'notifications', 'localNetwork']

export function missingPermissions(perms: PhonePermissions | undefined): PermissionKey[] {
  return perms ? PERMISSION_KEYS.filter((k) => !perms[k]) : []
}

/**
 * What Android allows right now, and a way to ask. The answer is read again whenever the app comes
 * back to the front, because the exact-alarm switch lives on a settings page the user leaves the app for.
 */
export function usePhonePermissions() {
  const phone = useSnapshotSource().phone
  const [permissions, setPermissions] = useState<PhonePermissions>()
  const [error, setError] = useState<Error>()
  const [asking, setAsking] = useState(false)
  const alive = useRef(true)

  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
    }
  }, [])

  const refresh = useCallback(() => {
    phone?.permissions().then(
      (next) => {
        if (!alive.current) return
        setPermissions(next)
        setError(undefined)
      },
      (err: unknown) => {
        if (alive.current) setError(new Error(errorMessage(err, 'Could not check the permissions.')))
      },
    )
  }, [phone])

  useEffect(() => {
    refresh()
    const onVisible = () => {
      if (document.visibilityState === 'visible') refresh()
    }
    document.addEventListener('visibilitychange', onVisible)
    window.addEventListener('focus', refresh)
    return () => {
      document.removeEventListener('visibilitychange', onVisible)
      window.removeEventListener('focus', refresh)
    }
  }, [refresh])

  /** Resolves with what is allowed afterwards. Rejects with the reason if Android could not be asked. */
  const ask = useCallback(async (): Promise<PhonePermissions | undefined> => {
    if (!phone) return undefined
    setAsking(true)
    try {
      const next = await phone.askPermissions()
      if (alive.current) setPermissions(next)
      return next
    } finally {
      if (alive.current) setAsking(false)
    }
  }, [phone])

  return { permissions, error, asking, ask, refresh, missing: missingPermissions(permissions) }
}
