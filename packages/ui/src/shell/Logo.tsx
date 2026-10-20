import { BellRing } from 'lucide-react'
import { cn } from '@/lib/utils'

/** The Cooldown mark: a bell on a tide-blue tile. */
export function Logo({ className }: { className?: string }) {
  return (
    <span
      aria-hidden="true"
      className={cn('grid size-9 shrink-0 place-items-center rounded-lg bg-primary text-primary-foreground', className)}
    >
      <BellRing className="size-5" />
    </span>
  )
}
