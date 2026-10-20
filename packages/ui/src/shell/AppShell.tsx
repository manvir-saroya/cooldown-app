/**
 * App frame. Layout adapted from the Watermelon Capitalio dashboard shell
 * (SidebarProvider + inset content panel with its own scroll area + topbar),
 * extended with a mobile bottom navigation and skip link.
 */
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react'
import { toast } from 'sonner'
import { SidebarInset, SidebarProvider } from '@/components/ui/sidebar'
import { useSnapshotSource, useSnapshots } from '@/data/SnapshotSource'
import { usePairedPcs } from '@/hooks/use-phone'
import { providerInfo } from '@/lib/providers'
import { useRoute } from '@/state/router'
import { useNowReader } from '@/state/now'
import { AppSidebar } from './AppSidebar'
import { BottomNav } from './BottomNav'
import { pageTitle } from './nav'
import { TopBar } from './TopBar'

export function AppShell({ children }: { children: ReactNode }) {
  const route = useRoute()
  const source = useSnapshotSource()
  const { reload, data } = useSnapshots()
  const readNow = useNowReader()
  // Undefined until the first readings arrive, so "Refreshed just now" is never shown while loading or
  // after a failed load. It moves on every change the source reports (a poll, a manual refresh).
  const [refreshedAt, setRefreshedAt] = useState<number>()
  const loaded = data !== undefined
  useEffect(() => {
    if (loaded) setRefreshedAt((prev) => prev ?? readNow())
  }, [loaded, readNow])
  useEffect(() => source.subscribe(() => setRefreshedAt((prev) => (prev === undefined ? prev : readNow()))), [source, readNow])
  const [refreshing, setRefreshing] = useState(false)
  const scrollRef = useRef<HTMLDivElement>(null)
  // Where the page was scrolled to when the user followed a link out of it, so Back puts them there again.
  const leftAt = useRef(new Map<string, number>())

  // On every route change: scroll to top (or back to where the user left the page), move focus to the page heading, and set the document title.
  const onPhone = Boolean(source.phone)
  // A phone with no PC yet has nothing that was refreshed: say so instead of "Your PC · Refreshed just now".
  const pcs = usePairedPcs()
  const unpaired = onPhone && pcs.data?.length === 0
  const title = pageTitle(route, route.name === 'provider' ? providerInfo(route.provider).name : undefined, onPhone)
  const routeKey = route.name === 'provider' ? `provider:${route.provider}` : route.name
  const lastRouteKey = useRef(routeKey)
  useEffect(() => {
    document.title = `${title} · Cooldown`
    if (lastRouteKey.current === routeKey) return
    lastRouteKey.current = routeKey
    scrollRef.current?.scrollTo({ top: leftAt.current.get(routeKey) ?? 0 })
    leftAt.current.delete(routeKey)
    // Focus the new page's heading on navigation, but not on first load: that would skip past
    // the skip link and the navigation for keyboard and screen reader users.
    document.getElementById('page-title')?.focus({ preventScroll: true })
  }, [routeKey, title])

  const onRefresh = useCallback(async () => {
    setRefreshing(true)
    try {
      await source.refresh()
      setRefreshedAt(readNow())
      reload()
    } catch (err) {
      if (onPhone) {
        toast.error('Could not reach your PC', {
          description: `${err instanceof Error && err.message ? err.message : 'Check that it is on and on the same Wi-Fi.'} The last readings are still shown.`,
        })
      } else {
        toast.error('Could not refresh readings', { description: 'The adapters did not answer. The last good values are still shown.' })
      }
    } finally {
      setRefreshing(false)
    }
  }, [source, reload, onPhone, readNow])

  const backTo = route.name === 'provider' ? { to: { name: 'overview' } as const, label: 'Back to overview' } : undefined

  return (
    <SidebarProvider className="h-dvh min-h-0 overflow-hidden lg:bg-sidebar">
      <a
        href="#main-content"
        onClick={(e) => {
          e.preventDefault()
          document.getElementById('main-content')?.focus()
        }}
        className="sr-only z-50 rounded-md bg-primary px-4 py-2 text-primary-foreground focus:not-sr-only focus:fixed focus:top-3 focus:left-3"
      >
        Skip to content
      </a>
      <AppSidebar route={route} />
      <SidebarInset className="min-h-0 min-w-0 overflow-hidden lg:m-2 lg:ml-0 lg:rounded-xl lg:border">
        <TopBar
          title={title}
          sourceLabel={unpaired ? 'Not paired yet' : source.label}
          refreshedAt={unpaired ? undefined : refreshedAt}
          refreshing={refreshing}
          onRefresh={onRefresh}
          backTo={backTo}
        />
        <div
          ref={scrollRef}
          onClickCapture={(e) => {
            if (e.target instanceof Element && e.target.closest('a')) leftAt.current.set(lastRouteKey.current, scrollRef.current?.scrollTop ?? 0)
          }}
          className="min-h-0 flex-1 overflow-y-auto overscroll-contain"
        >
          <div id="main-content" tabIndex={-1} className="mx-auto w-full max-w-6xl px-4 pt-4 pb-8 outline-none lg:px-8 lg:pt-4 lg:pb-12">
            {children}
          </div>
        </div>
        <BottomNav route={route} />
      </SidebarInset>
    </SidebarProvider>
  )
}
