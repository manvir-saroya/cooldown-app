import * as React from "react"
import * as m from "motion/react-m"
import { Progress as ProgressPrimitive } from "radix-ui"
import { cn } from "@/lib/utils"

/**
 * shadcn Progress with a Motion-driven fill. The fill is a full-width element
 * revealed by clip-path, so patterned fills (hatch) keep their shape while the
 * value changes, and a new value retargets smoothly instead of restarting.
 */
function Progress({
  className,
  indicatorClassName,
  value,
  children,
  ...props
}: React.ComponentProps<typeof ProgressPrimitive.Root> & {
  indicatorClassName?: string
}) {
  const clamped = Math.min(100, Math.max(0, value ?? 0))
  return (
    <ProgressPrimitive.Root
      data-slot="progress"
      value={value}
      className={cn(
        "relative flex h-1 w-full items-center overflow-hidden rounded-full bg-muted",
        className
      )}
      {...props}
    >
      <ProgressPrimitive.Indicator asChild>
        <m.div
          data-slot="progress-indicator"
          className={cn("absolute inset-0 bg-primary", indicatorClassName)}
          initial={{ clipPath: "inset(0 100% 0 0 round 9999px)" }}
          animate={{ clipPath: `inset(0 ${100 - clamped}% 0 0 round 9999px)` }}
          transition={{ type: "spring", duration: 0.7, bounce: 0 }}
        />
      </ProgressPrimitive.Indicator>
      {children}
    </ProgressPrimitive.Root>
  )
}

export { Progress }
