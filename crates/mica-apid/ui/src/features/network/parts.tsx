import type { ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import { formatAge } from '@/i18n/format'
import { band, channelOf } from '@/lib/wifi-scan'
import { cn } from '@/shared/lib/utils'
import { StatusBadge, type StatusTone } from '@/shared/components/status-badge'
import { Card } from '@/shared/components/ui/card'
import type { IfaceStatus, IfaceView, WifiView } from './view'

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
    note: (status: IfaceStatus) => {
      if (!status.note) return undefined
      const age = status.note.params?.handshakeAge
      return t(status.note.key, { ...status.note.params, ...(typeof age === 'number' ? { handshake: formatAge(age * 1000, t) } : {}) })
    },
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

/// Wi-Fi readings in words: a signal with its strength, a frequency as its
/// band and channel, the supplicant's key management as a security type.
export function useWifiText() {
  const { t } = useTranslation()
  return {
    signal: (dbm: number) => t('network.wifi.signalValue', { dbm, quality: t(`network.wifi.signalQuality.${dbm >= -60 ? 'strong' : dbm >= -75 ? 'medium' : 'weak'}`) }),
    bandChannel: (frequencyMhz?: number) => {
      const which = band(frequencyMhz)
      const channel = channelOf(frequencyMhz)
      return which && channel ? t('network.wifi.bandChannel', { band: which, channel }) : undefined
    },
    /// The channel the access point runs on, with its band when hostapd said.
    apChannel: (wifi: WifiView) => {
      const which = band(wifi.apFrequencyMhz)
      return which ? t('network.wifi.channelOn', { channel: wifi.apChannel, band: which }) : t('network.wifi.channelValue', { channel: wifi.apChannel })
    },
    security: (keyManagement: string) => keyManagement === 'NONE' ? t('network.wifi.securities.open') : keyManagement === 'SAE' ? t('network.wifi.securities.sae') : keyManagement,
  }
}

/// A block of a network page, as wide as the page: its title, explanation
/// and actions head the card above a rule. A table's rows meet the card's
/// edges (`flush`); a block being edited is outlined; a danger block's title
/// is red.
export function Section({ title, description, action, tone = 'default', editing, flush, footer, children }: {
  title: string
  description?: string
  action?: ReactNode
  tone?: 'default' | 'danger'
  editing?: boolean
  flush?: boolean
  footer?: ReactNode
  children?: ReactNode
}) {
  return (
    <Card className={cn('gap-0 py-0', editing && 'ring-2 ring-primary/40')}>
      <header className="flex flex-col gap-3 border-b border-border p-4 sm:flex-row sm:items-start sm:justify-between">
        <div className="grid min-w-0 gap-1">
          <h2 className={cn('text-base font-semibold', tone === 'danger' && 'text-destructive')}>{title}</h2>
          {description ? <p className="text-[0.8125rem] text-muted-foreground">{description}</p> : null}
        </div>
        {action ? <div className="flex shrink-0 flex-wrap items-center gap-2">{action}</div> : null}
      </header>
      {flush ? children : <div className="flex flex-col gap-4 p-4">{children}</div>}
      {footer ? <div className="border-t border-border p-3">{footer}</div> : null}
    </Card>
  )
}

/// A titled group of fields inside a block.
export function Group({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="grid gap-2">
      <h3 className="text-sm font-semibold">{title}</h3>
      {children}
    </section>
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

const CAN_STATES: Record<string, string> = {
  'error-active': 'normal', 'error-warning': 'warning', 'error-passive': 'passive', 'bus-off': 'busOff', stopped: 'stopped', sleeping: 'sleeping',
}

/// A CAN controller's state and its bit rate in words.
export function useCanText() {
  const { t } = useTranslation()
  return {
    state: (busState: string) => CAN_STATES[busState] ? t(`network.can.states.${CAN_STATES[busState]}`, { state: busState }) : busState,
    bitrate: (bitrate: number) => t('network.status.notes.bitrate', { kbps: bitrate / 1000 }),
  }
}
