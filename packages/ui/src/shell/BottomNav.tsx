/**
 * Mobile bottom navigation. Hand-built because the registry shells ship a drawer
 * or hamburger, not a thumb-reach tab bar. Shown below 1024px; the sidebar takes
 * over from there.
 */
import * as m from 'motion/react-m'
import { cn } from '@/lib/utils'
import { Link, type Route } from '@/state/router'
import { useAlerts } from '@/state/alerts'
import { NAV_ITEMS } from './nav'

export function BottomNav({ route }: { route: Route }) {
  const { alerts } = useAlerts()
  const armed = alerts.filter((a) => a.enabled && a.rangAt === undefined).length

  return (
    <nav
      aria-label="Primary"
      className="shrink-0 border-t bg-sidebar pb-[max(env(safe-area-inset-bottom),var(--nav-inset,0px))] text-sidebar-foreground lg:hidden"
    >
      <ul className="mx-auto grid max-w-xl grid-cols-4">
        {NAV_ITEMS.map((item) => {
          const active = item.isActive(route)
          const Icon = item.icon
          return (
            <li key={item.key}>
              <Link
                to={item.route}
                aria-current={active ? 'page' : undefined}
                className={cn(
                  'group flex min-h-14 flex-col items-center justify-center gap-0.5 px-2 py-1.5 text-xs transition-colors',
                  'focus-visible:-outline-offset-2',
                  active ? 'font-semibold text-sidebar-accent-foreground' : 'text-muted-foreground',
                )}
              >
                <span className="relative grid h-8 w-14 place-items-center">
                  {active && (
                    <m.span
                      layoutId="bottom-nav-active"
                      className="absolute inset-0 rounded-full bg-sidebar-accent"
                      transition={{ type: 'spring', duration: 0.35, bounce: 0.15 }}
                    />
                  )}
                  <Icon aria-hidden="true" className="relative size-5" />
                  {item.key === 'alerts' && armed > 0 && (
                    <span
                      aria-hidden="true"
                      className="num absolute top-0 right-2 grid min-w-4 place-items-center rounded-full bg-primary px-1 text-xs leading-4 font-semibold text-primary-foreground"
                    >
                      {armed}
                    </span>
                  )}
                </span>
                {item.label}
                {item.key === 'alerts' && armed > 0 && <span className="sr-only">, {armed} {armed === 1 ? 'bell' : 'bells'} set</span>}
              </Link>
            </li>
          )
        })}
      </ul>
    </nav>
  )
}
