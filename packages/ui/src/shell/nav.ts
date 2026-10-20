import { BellRing, LayoutGrid, QrCode, Settings, type LucideIcon } from 'lucide-react'
import type { Route } from '@/state/router'

export type NavItem = {
  key: string
  label: string
  route: Route
  icon: LucideIcon
  /** True when `current` should light this item up. */
  isActive: (current: Route) => boolean
}

export const NAV_ITEMS: NavItem[] = [
  {
    key: 'overview',
    label: 'Overview',
    route: { name: 'overview' },
    icon: LayoutGrid,
    isActive: (r) => r.name === 'overview' || r.name === 'provider',
  },
  { key: 'alerts', label: 'Alerts', route: { name: 'alerts' }, icon: BellRing, isActive: (r) => r.name === 'alerts' },
  { key: 'pairing', label: 'Pair', route: { name: 'pairing' }, icon: QrCode, isActive: (r) => r.name === 'pairing' },
  { key: 'settings', label: 'Settings', route: { name: 'settings' }, icon: Settings, isActive: (r) => r.name === 'settings' },
]

export function pageTitle(route: Route, providerName?: string, onPhone = false): string {
  switch (route.name) {
    case 'overview':
      return 'Overview'
    case 'provider':
      return providerName ?? route.provider
    case 'alerts':
      return 'Alerts'
    case 'settings':
      return 'Settings'
    case 'pairing':
      return onPhone ? 'Pair with your PC' : 'Pair a device'
    case 'not-found':
      return 'Page not found'
  }
}
