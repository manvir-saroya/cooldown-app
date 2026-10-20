/**
 * A tiny hash router. Hash URLs work unchanged in a Tauri window, in a plain
 * browser tab served from 127.0.0.1 and on Android, with no server rewrite rules.
 */
import { useEffect, useState, type AnchorHTMLAttributes, type ReactNode } from 'react'
import type { WindowKind } from '@cooldown/snapshot-schema'

export type Route =
  | { name: 'overview' }
  | { name: 'provider'; provider: string; window?: WindowKind }
  | { name: 'alerts' }
  | { name: 'settings' }
  | { name: 'pairing' }
  | { name: 'not-found'; path: string }

const WINDOWS: WindowKind[] = ['session_5h', 'weekly', 'monthly', 'daily', 'balance']

export function parseHash(hash: string): Route {
  const path = hash.replace(/^#/, '').split('?')[0] ?? ''
  const parts = path.split('/').filter(Boolean).map(decodeURIComponent)
  const [first, second, third] = parts
  if (!first) return { name: 'overview' }
  if (first === 'alerts' && parts.length === 1) return { name: 'alerts' }
  if (first === 'settings' && parts.length === 1) return { name: 'settings' }
  if (first === 'pairing' && parts.length === 1) return { name: 'pairing' }
  if (first === 'provider' && second && parts.length <= 3) {
    const window = WINDOWS.find((w) => w === third)
    return { name: 'provider', provider: second, window }
  }
  return { name: 'not-found', path: `/${parts.join('/')}` }
}

export function hrefFor(route: Route): string {
  switch (route.name) {
    case 'overview':
      return '#/'
    case 'alerts':
      return '#/alerts'
    case 'settings':
      return '#/settings'
    case 'pairing':
      return '#/pairing'
    case 'provider':
      return `#/provider/${encodeURIComponent(route.provider)}${route.window ? `/${route.window}` : ''}`
    case 'not-found':
      return '#/'
  }
}

export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(() => parseHash(location.hash))
  useEffect(() => {
    const onChange = () => setRoute(parseHash(location.hash))
    window.addEventListener('hashchange', onChange)
    return () => window.removeEventListener('hashchange', onChange)
  }, [])
  return route
}

export function navigate(route: Route, options: { replace?: boolean } = {}): void {
  const href = hrefFor(route)
  if (options.replace) {
    history.replaceState(null, '', href)
    window.dispatchEvent(new HashChangeEvent('hashchange'))
  } else {
    location.hash = href
  }
}

export function Link({
  to,
  children,
  ...props
}: { to: Route; children: ReactNode } & Omit<AnchorHTMLAttributes<HTMLAnchorElement>, 'href'>) {
  return (
    <a href={hrefFor(to)} {...props}>
      {children}
    </a>
  )
}
