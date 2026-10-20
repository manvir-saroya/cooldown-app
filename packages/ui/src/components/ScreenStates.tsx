import { CircleAlert, type LucideIcon } from 'lucide-react'
import type { ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { cn } from '@/lib/utils'

export function LoadingCards({ count = 6 }: { count?: number }) {
  return (
    <div role="status" aria-label="Loading readings…" className="grid grid-cols-1 gap-4 sm:grid-cols-2 xl:grid-cols-3">
      {Array.from({ length: count }, (_, i) => (
        <div key={i} className="flex flex-col gap-4 rounded-xl border bg-card p-5">
          <div className="flex items-center gap-3">
            <Skeleton className="size-10 rounded-lg" />
            <div className="flex-1 space-y-2">
              <Skeleton className="h-4 w-1/3" />
              <Skeleton className="h-3 w-1/2" />
            </div>
          </div>
          <Skeleton className="h-10 w-2/3" />
          <Skeleton className="h-3.5 w-full" />
          <Skeleton className="h-11 w-full" />
        </div>
      ))}
    </div>
  )
}

export function ErrorState({ title, detail, onRetry }: { title: string; detail?: string; onRetry?: () => void }) {
  return (
    <div role="alert" className="flex flex-col items-start gap-3 rounded-xl border border-shoal/50 bg-card p-5">
      <p className="flex items-center gap-2 text-base font-semibold text-shoal-ink">
        <CircleAlert aria-hidden="true" className="size-5" />
        {title}
      </p>
      {detail && <p className="text-sm text-muted-foreground [overflow-wrap:anywhere]">{detail}</p>}
      {onRetry && (
        <Button variant="outline" onClick={onRetry}>
          Try again
        </Button>
      )}
    </div>
  )
}

export function EmptyState({
  icon: Icon,
  title,
  children,
  action,
  className,
}: {
  icon: LucideIcon
  title: string
  children: ReactNode
  action?: ReactNode
  className?: string
}) {
  return (
    <div className={cn('flex flex-col items-start gap-3 rounded-xl border border-dashed bg-card p-6', className)}>
      <span aria-hidden="true" className="grid size-10 place-items-center rounded-lg bg-secondary text-secondary-foreground">
        <Icon className="size-5" />
      </span>
      <div className="space-y-1">
        <p className="text-base font-semibold">{title}</p>
        <p className="max-w-prose text-sm text-muted-foreground [overflow-wrap:anywhere]">{children}</p>
      </div>
      {action}
    </div>
  )
}
