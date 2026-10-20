/**
 * Desktop sidebar. Built on the shadcn Sidebar primitives that the Watermelon
 * Capitalio dashboard shell installs (brand header, grouped nav, footer), with
 * the finance content, avatar and "Premium" badge replaced by Cooldown's own.
 * Shown from 1024px; below that the bottom navigation takes over.
 */
import { Lock } from 'lucide-react'
import { useMemo } from 'react'
import type { Status } from '@cooldown/snapshot-schema'
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from '@/components/ui/sidebar'
import { useSnapshotSource, useSnapshots } from '@/data/SnapshotSource'
import { providerInfo } from '@/lib/providers'
import { effectiveStatus, SEVERITY } from '@/lib/snapshot-view'
import { cn } from '@/lib/utils'
import { useAlerts } from '@/state/alerts'
import { shallowEqual, useNowSelector } from '@/state/now'
import { Link, type Route } from '@/state/router'
import { Logo } from './Logo'
import { NAV_ITEMS } from './nav'

const STATUS_WORD: Record<Status, string | undefined> = {
  ok: undefined,
  stale: 'Stale',
  degraded: 'Degraded',
  unavailable: 'Unavailable',
}

const navButtonClass =
  'h-11 gap-3 rounded-lg px-3 text-base text-sidebar-foreground data-active:bg-sidebar-accent data-active:font-semibold data-active:text-sidebar-accent-foreground'

export function AppSidebar({ route }: { route: Route }) {
  const { isMobile } = useSidebar()
  const onPhone = Boolean(useSnapshotSource().phone)
  const { data: snapshots } = useSnapshots()
  const { alerts } = useAlerts()
  const providers = useMemo(() => Array.from(new Set((snapshots ?? []).map((s) => s.provider))), [snapshots])
  // The worst status per provider. Only changes when a reset passes, so the sidebar does not render every second.
  const statuses = useNowSelector(
    (now) =>
      providers.map((provider) =>
        (snapshots ?? [])
          .filter((s) => s.provider === provider)
          .map((s) => effectiveStatus(s, now).status)
          .reduce<Status>((a, b) => (SEVERITY[b] > SEVERITY[a] ? b : a), 'ok'),
      ),
    shallowEqual,
  )
  if (isMobile) return null

  const armed = alerts.filter((a) => a.enabled && a.rangAt === undefined).length

  return (
    <Sidebar className="hidden shrink-0 bg-transparent p-2 pr-0 lg:flex">
      <SidebarHeader className="px-4 pt-5 pb-2">
        <Link
          to={{ name: 'overview' }}
          className="flex items-center gap-3 rounded-lg outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <Logo />
          <span className="text-lg font-semibold text-sidebar-accent-foreground">Cooldown</span>
        </Link>
      </SidebarHeader>

      <SidebarContent className="gap-2 px-3 pb-4">
        <SidebarGroup className="p-0">
          <SidebarGroupContent>
            <SidebarMenu className="gap-1">
              {NAV_ITEMS.map((item) => {
                const Icon = item.icon
                const active = item.isActive(route)
                return (
                  <SidebarMenuItem key={item.key}>
                    <SidebarMenuButton asChild isActive={active} className={navButtonClass}>
                      <Link to={item.route} aria-current={active ? 'page' : undefined}>
                        <Icon aria-hidden="true" />
                        <span>{item.key === 'pairing' ? (onPhone ? 'Pair with your PC' : 'Pair a device') : item.label}</span>
                        {item.key === 'alerts' && armed > 0 && (
                          <span className="sr-only">, {armed} {armed === 1 ? 'bell' : 'bells'} set</span>
                        )}
                      </Link>
                    </SidebarMenuButton>
                    {item.key === 'alerts' && armed > 0 && (
                      <SidebarMenuBadge aria-hidden="true" className="num text-sidebar-foreground peer-data-[size=default]/menu-button:top-3">
                        {armed}
                      </SidebarMenuBadge>
                    )}
                  </SidebarMenuItem>
                )
              })}
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>

        <SidebarGroup className="p-0">
          <SidebarGroupLabel className="h-9 px-3 text-sm font-medium text-muted-foreground">Providers</SidebarGroupLabel>
          <SidebarGroupContent>
            <SidebarMenu className="gap-1">
              {providers.map((id, i) => {
                const active = route.name === 'provider' && route.provider === id
                const status = statuses[i] ?? 'ok'
                const word = STATUS_WORD[status]
                return (
                  <SidebarMenuItem key={id}>
                    <SidebarMenuButton asChild isActive={active} className={cn(navButtonClass, 'justify-between')}>
                      <Link to={{ name: 'provider', provider: id }} aria-current={active ? 'page' : undefined}>
                        <span className="truncate">{providerInfo(id).name}</span>
                        {word && (
                          <span
                            className={cn(
                              'text-xs font-medium',
                              status === 'unavailable' ? 'text-muted-foreground' : 'text-sand-ink',
                            )}
                          >
                            {word}
                          </span>
                        )}
                      </Link>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                )
              })}
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
      </SidebarContent>

      <SidebarFooter className="px-4 pt-0 pb-5">
        <p className="flex items-start gap-2 text-sm text-muted-foreground">
          <Lock aria-hidden="true" className="mt-0.5 size-4 shrink-0" />
          <span>
            {onPhone
              ? 'Runs on your devices. No account, no cloud. Keys stay on your PC.'
              : 'Runs on your devices. No account, no cloud, keys never leave this PC.'}
          </span>
        </p>
      </SidebarFooter>
    </Sidebar>
  )
}
