import { lazy, Suspense } from 'react'
import { LazyMotion, MotionConfig } from 'motion/react'
import { SearchX } from 'lucide-react'
import { EmptyState, LoadingCards } from '@/components/ScreenStates'
import { Button } from '@/components/ui/button'
import { Toaster } from '@/components/ui/sonner'
import { SnapshotSourceProvider, type SnapshotSource } from '@/data/SnapshotSource'
import { AlertsScreen } from '@/screens/Alerts'
import { OverviewScreen } from '@/screens/Overview'
import { SettingsScreen } from '@/screens/Settings'
import { AppShell } from '@/shell/AppShell'
import { AlertsProvider } from '@/state/alerts'
import { NowProvider } from '@/state/now'
import { Link, useRoute } from '@/state/router'
import { ThemeProvider } from '@/state/theme'

// The Motion engine is fetched after first paint; components use the slim `m` element until it arrives.
const loadMotionFeatures = () => import('@/lib/motion-features').then((module) => module.default)

// The chart (Recharts) and the QR code are only needed on two screens, so they load on demand.
const ProviderScreen = lazy(() => import('@/screens/Provider').then((m) => ({ default: m.ProviderScreen })))
const PairingScreen = lazy(() => import('@/screens/Pairing').then((m) => ({ default: m.PairingScreen })))

export interface AppProps {
  /** Where data comes from. The only thing that changes between sample, Tauri and HTTP builds. */
  source: SnapshotSource
  /** Freeze the clock (tests, screenshots). Omit for a live clock. */
  fixedNow?: number
  /** Snapshot ids that start with a bell already set. */
  initialArmed?: string[]
}

export function App({ source, fixedNow, initialArmed }: AppProps) {
  return (
    // reducedMotion="user": transform and layout animation switch off when the OS asks for less motion.
    <MotionConfig reducedMotion="user">
      <LazyMotion features={loadMotionFeatures} strict>
        <ThemeProvider>
          <NowProvider fixed={fixedNow}>
            <SnapshotSourceProvider source={source}>
              <AlertsProvider initialArmed={initialArmed}>
                <AppShell>
                  <Routes />
                </AppShell>
                <Toaster position="top-center" />
              </AlertsProvider>
            </SnapshotSourceProvider>
          </NowProvider>
        </ThemeProvider>
      </LazyMotion>
    </MotionConfig>
  )
}

function Routes() {
  const route = useRoute()
  switch (route.name) {
    case 'overview':
      return <OverviewScreen />
    case 'provider':
      return (
        <Suspense fallback={<LoadingCards count={1} />}>
          <ProviderScreen provider={route.provider} window={route.window} />
        </Suspense>
      )
    case 'alerts':
      return <AlertsScreen />
    case 'settings':
      return <SettingsScreen />
    case 'pairing':
      return (
        <Suspense fallback={<LoadingCards count={1} />}>
          <PairingScreen />
        </Suspense>
      )
    case 'not-found':
      return (
        <EmptyState
          icon={SearchX}
          title="That page does not exist"
          action={
            <Button asChild>
              <Link to={{ name: 'overview' }}>Go to overview</Link>
            </Button>
          }
        >
          There is nothing at {route.path}. Use the navigation to get back to your limits.
        </EmptyState>
      )
  }
}
