import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { cn } from '@/shared/lib/utils'
import { StatusBadge, type StatusTone } from '@/shared/components/status-badge'
import type { IfaceStatus, IfaceView } from './view'

const LED: Record<StatusTone, string> = {
  neutral: 'bg-muted-foreground/50',
  accent: 'bg-primary',
  success: 'bg-success',
  warning: 'bg-warning',
  danger: 'bg-destructive',
}

export const TEXT: Record<StatusTone, string> = {
  neutral: 'text-muted-foreground',
  accent: 'text-primary',
  success: 'text-success',
  warning: 'text-warning',
  danger: 'text-destructive',
}

export function Led({ tone, className }: { tone: StatusTone; className?: string }) {
  return <span className={cn('inline-block size-2 shrink-0 rounded-full', LED[tone], className)} aria-hidden="true" />
}

export function useStatusText() {
  const { t } = useTranslation()
  return {
    label: (status: IfaceStatus) => t(`network.status.${status.label}`),
    note: (status: IfaceStatus) => status.note ? t(status.note.key, status.note.params) : undefined,
  }
}

export function StatusLine({ status, compact }: { status: IfaceStatus; compact?: boolean }) {
  const text = useStatusText()
  const note = text.note(status)
  return (
    <span className="flex flex-wrap items-center gap-2">
      <StatusBadge tone={status.tone}>{text.label(status)}</StatusBadge>
      {note && !compact ? <span className="text-sm text-muted-foreground">{note}</span> : null}
    </span>
  )
}

/// A short label beside a name: 出口, 当前会话, 镜像默认, 待应用.
export function Tag({ tone = 'neutral', children }: { tone?: 'neutral' | 'info' | 'pending' | 'exit'; children: ReactNode }) {
  return (
    <span className={cn(
      'inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 text-xs font-medium whitespace-nowrap',
      tone === 'info' && 'border-primary/30 bg-primary/10 text-primary',
      tone === 'pending' && 'border-warning/40 bg-warning-background text-warning',
      tone === 'exit' && 'border-success/30 bg-success-background text-success',
      tone === 'neutral' && 'text-muted-foreground',
    )}>
      {children}
    </span>
  )
}

export function useKindLabel() {
  const { t } = useTranslation()
  return (iface: Pick<IfaceView, 'kind' | 'entry'>) => iface.kind === 'vlan'
    ? t('network.kinds.vlanId', { id: iface.entry.vlan?.id ?? '' })
    : t(`network.kinds.${iface.kind}`)
}

/// The interface groups of the list, in their order.
export const ROLES = ['phys', 'lan', 'wlan', 'vpn', 'field'] as const
export type Role = typeof ROLES[number]

export function roleOf(iface: Pick<IfaceView, 'kind'>): Role {
  switch (iface.kind) {
    case 'physical': return 'phys'
    case 'wifi': return 'wlan'
    case 'wireguard': return 'vpn'
    case 'can': return 'field'
    default: return 'lan'
  }
}
