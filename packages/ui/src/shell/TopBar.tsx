import { ArrowLeft, RefreshCw } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { cn } from '@/lib/utils'
import { useAge } from '@/state/now'
import { Link } from '@/state/router'
import { Logo } from './Logo'
import { ThemeMenu } from './ThemeMenu'

export function TopBar({
  title,
  sourceLabel,
  refreshedAt,
  refreshing,
  onRefresh,
  backTo,
}: {
  title: string
  /** For example "Sample data". Shown beside the refresh time so sample data is never mistaken for real. */
  sourceLabel: string
  /** When the readings were last refreshed. Undefined until the first load has finished. */
  refreshedAt: number | undefined
  refreshing: boolean
  onRefresh: () => void
  /** When set, shows a back link instead of the logo on small screens. */
  backTo?: { to: Parameters<typeof Link>[0]['to']; label: string }
}) {
  const refreshed = useAge(refreshedAt)
  return (
    <header className="shrink-0 border-b pt-[max(0.5rem,env(safe-area-inset-top))] pb-2 lg:border-b-0 lg:pt-6 lg:pb-2">
      {/* Same width and side padding as the page content (AppShell), so the title lines up with it on wide windows. */}
      <div className="mx-auto flex w-full max-w-6xl flex-wrap items-center gap-x-3 gap-y-1 px-4 lg:px-8">
        {backTo ? (
          <Button asChild variant="ghost" size="icon" className="-ml-2 lg:hidden">
            <Link to={backTo.to} aria-label={backTo.label}>
              <ArrowLeft aria-hidden="true" />
            </Link>
          </Button>
        ) : (
          <Logo className="size-8 lg:hidden" />
        )}
        <div className="min-w-[6rem] flex-1">
          <h1
            id="page-title"
            tabIndex={-1}
            className="line-clamp-2 text-lg leading-tight font-semibold break-words outline-none lg:text-2xl"
          >
            {title}
          </h1>
          <p className="num truncate text-xs text-muted-foreground lg:text-sm">
            <span className="font-medium text-foreground">{sourceLabel}</span>
            {refreshed && (
              <>
                <span aria-hidden="true"> · </span>
                Refreshed {refreshed}
              </>
            )}
          </p>
        </div>
        <div className="ml-auto flex shrink-0 items-center gap-1">
          {/* aria-disabled, not disabled: a disabled button loses keyboard focus and it would drop to the page. */}
          <Button
            variant="ghost"
            size="icon"
            onClick={() => {
              if (!refreshing) onRefresh()
            }}
            aria-label="Refresh readings"
            aria-disabled={refreshing}
            aria-busy={refreshing}
          >
            <RefreshCw aria-hidden="true" className={cn(refreshing && 'animate-spin')} />
          </Button>
          <ThemeMenu />
        </div>
      </div>
    </header>
  )
}
