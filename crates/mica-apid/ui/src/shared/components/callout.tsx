import type { ReactNode } from 'react'
import { CircleAlert, CircleCheck, Info, OctagonX } from 'lucide-react'
import { Alert, AlertDescription, AlertTitle } from '@/shared/components/ui/alert'
import { cn } from '@/shared/lib/utils'

export type Tone = 'neutral' | 'accent' | 'success' | 'warning' | 'danger'

const icons = {
  neutral: Info,
  accent: Info,
  success: CircleCheck,
  warning: CircleAlert,
  danger: OctagonX,
} as const

const variants = {
  neutral: 'default',
  accent: 'default',
  success: 'success',
  warning: 'warning',
  danger: 'destructive',
} as const

/// A condition the device is in, stated where it applies.
export function Callout({ tone = 'neutral', title, children, className }: {
  tone?: Tone
  title?: string
  children?: ReactNode
  className?: string
}) {
  const Icon = icons[tone]
  return (
    <Alert
      variant={variants[tone]}
      role={tone === 'danger' ? 'alert' : 'status'}
      className={cn(tone === 'neutral' || tone === 'accent' ? 'border-border' : undefined, className)}
    >
      <Icon aria-hidden="true" />
      {title ? <AlertTitle>{title}</AlertTitle> : null}
      {children ? <AlertDescription>{children}</AlertDescription> : null}
    </Alert>
  )
}
