import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { FolderTree, HardDrive, Layers, ShieldQuestion, Usb } from 'lucide-react'
import { api } from '@/shared/lib/http'
import type { ExternalMedia, ExternalVolume, StorageBind, StorageMedium, StorageStatus, StorageTier } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { RowItem, RowList } from '@/shared/components/row-item'
import { StatusBadge, type StatusTone } from '@/shared/components/status-badge'
import { Button } from '@/shared/components/ui/button'
import { FactList } from '@/shared/components/fact-list'
import { Panel } from '@/shared/components/panel'
import { Spinner } from '@/shared/components/ui/spinner'
import { failureDetail } from '@/shared/feedback/toast'
import { formatBytes } from '@/shared/components/fact'

function useStorage() {
  return useQuery({
    queryKey: ['storage-status'],
    queryFn: () => api<StorageStatus>('/api/v1/storage/status'),
    refetchInterval: 30_000,
    retry: false,
  })
}

export function StoragePanel() {
  return (
    <div className="grid gap-3">
      <TiersPanel />
      <NamespacesPanel />
      <ExternalMediaPanel />
      <div className="grid gap-3 lg:grid-cols-2"><MediaPanel /><LifecyclePanel /></div>
    </div>
  )
}

export function TiersPanel() {
  const { t } = useTranslation()
  const status = useStorage()
  return (
    <Panel title={t('system.storage.tiers.title')} description={t('system.storage.tiers.description')} action={<Layers className="size-5 text-muted-foreground" />}>
      {status.isPending ? <p className="flex items-center gap-2 text-sm text-muted-foreground"><Spinner />{t('system.storage.tiers.loading')}</p> : null}
      <FactList facts={(status.data?.tiers ?? []).map((tier) => ({ id: tier.name, label: tier.name, value: tierSummary(tier, t) }))} />
      {status.error ? <Callout tone="danger" title={failureDetail(status.error, t('common.requestFailed'))} /> : null}
    </Panel>
  )
}

export function NamespacesPanel() {
  const { t } = useTranslation()
  const status = useStorage()
  const namespaces = status.data?.namespaces
  return (
    <Panel title={t('system.storage.namespaces.title')} description={t('system.storage.namespaces.description')} action={<FolderTree className="size-5 text-muted-foreground" />}>
      <FactList facts={(namespaces?.binds ?? []).map((bind) => ({ id: bind.name, label: bind.mount, value: bindSummary(bind, t) }))} />
      {/* One pool, two views. Said in the UI as well as the API, because a
          reader looking at two mounts will otherwise assume two capacities. */}
      {namespaces ? <Callout title={t('system.storage.namespaces.sharedPool', { tier: namespaces.sharedCapacityTier })} /> : null}
    </Panel>
  )
}

const VOLUME_TONES: Record<ExternalVolume['state'], StatusTone> = {
  mounted_readonly: 'success',
  unmounted: 'neutral',
  detected: 'warning',
  unsupported: 'neutral',
  failed: 'danger',
  removed: 'neutral',
  ejected: 'accent',
}

/// The USB sticks and SD cards plugged in now, one group per device. The list
/// is read every five seconds while the page is open, and each read is also
/// what mounts a newly plugged volume, read-only.
export function ExternalMediaPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const media = useQuery({
    queryKey: ['storage-media'],
    queryFn: () => api<ExternalMedia>('/api/v1/storage/media'),
    refetchInterval: 5_000,
    retry: false,
  })
  const refresh = () => void queryClient.invalidateQueries({ queryKey: ['storage-media'] })
  const post = (path: string) => api(path, { method: 'POST' }).finally(refresh)
  const devices = new Map<string, ExternalVolume[]>()
  for (const volume of media.data?.items ?? []) {
    devices.set(volume.deviceId, [...(devices.get(volume.deviceId) ?? []), volume])
  }
  return (
    <Panel title={t('system.storage.external.title')} description={t('system.storage.external.description')} action={<Usb className="size-5 text-muted-foreground" />}>
      {media.isPending ? <p className="flex items-center gap-2 text-sm text-muted-foreground"><Spinner />{t('system.storage.external.loading')}</p> : null}
      {[...devices].map(([deviceId, volumes]) => {
        const present = volumes.filter((volume) => volume.state !== 'removed')
        const ejected = present.length > 0 && present.every((volume) => volume.state === 'ejected')
        const name = t(`system.storage.external.transport.${volumes[0].transport}`)
        return (
          <section key={deviceId} className="grid gap-1" aria-label={name}>
            <div className="flex items-center justify-between gap-2">
              <h3 className="text-sm font-medium">{name}</h3>
              {ejected ? <StatusBadge tone="accent">{t('system.storage.external.safeToUnplug')}</StatusBadge> : null}
              {present.length > 0 && !ejected ? (
                <ConfirmDialog
                  trigger={<Button variant="outline" size="sm">{t('system.storage.external.eject')}</Button>}
                  tone="neutral"
                  title={t('system.storage.external.eject')}
                  description={t('system.storage.external.ejectConfirm')}
                  confirmLabel={t('system.storage.external.eject')}
                  success={t('system.storage.external.ejected')}
                  failure={t('system.storage.external.eject')}
                  onConfirm={() => post(`/api/v1/storage/media/devices/${encodeURIComponent(deviceId)}/eject`)}
                />
              ) : null}
            </div>
            <RowList>
              {volumes.map((volume) => (
                <RowItem
                  key={volume.id}
                  title={<span className="flex items-center gap-2">{volume.label ?? t('system.storage.external.noLabel')}<StatusBadge tone={VOLUME_TONES[volume.state]}>{t(`system.storage.external.state.${volume.state}`)}</StatusBadge></span>}
                  description={volumeSummary(volume, t)}
                  actions={volume.state === 'mounted_readonly' ? (
                    <ConfirmDialog
                      trigger={<Button variant="ghost" size="sm">{t('system.storage.external.unmount')}</Button>}
                      tone="neutral"
                      title={t('system.storage.external.unmount')}
                      description={t('system.storage.external.unmountConfirm')}
                      confirmLabel={t('system.storage.external.unmount')}
                      success={t('system.storage.external.unmounted')}
                      failure={t('system.storage.external.unmount')}
                      onConfirm={() => post(`/api/v1/storage/media/${encodeURIComponent(volume.id)}/unmount`)}
                    />
                  ) : null}
                />
              ))}
            </RowList>
          </section>
        )
      })}
      {media.data && media.data.items.length === 0 ? <Callout title={t('system.storage.external.none')} /> : null}
      {media.error ? <Callout tone="danger" title={failureDetail(media.error, t('common.requestFailed'))} /> : null}
    </Panel>
  )
}

/// A volume in one line: its filesystem, size and free space, and why it is in
/// its state when micad says.
function volumeSummary(volume: ExternalVolume, t: ReturnType<typeof useTranslation>['t']) {
  const parts = [volume.filesystem, formatBytes(volume.sizeBytes)]
  if (volume.availableBytes !== null) parts.push(t('system.storage.external.available', { available: formatBytes(volume.availableBytes) }))
  if (volume.reason) parts.push(t(`system.storage.external.reason.${volume.reason}`, { defaultValue: volume.reason }))
  return parts.join(' · ')
}

export function MediaPanel() {
  const { t } = useTranslation()
  const status = useStorage()
  return (
    <Panel title={t('system.storage.media.title')} description={t('system.storage.media.description')} action={<HardDrive className="size-5 text-muted-foreground" />}>
      <FactList facts={(status.data?.media ?? []).map((medium) => ({
        id: medium.name,
        label: `${medium.name}${medium.sizeBytes ? ` · ${formatBytes(medium.sizeBytes)}` : ''}`,
        value: mediumHealth(medium, t),
      }))} />
      {status.data && status.data.media.length === 0 ? <Callout tone="warning" title={t('system.storage.media.none')} /> : null}
    </Panel>
  )
}

export function LifecyclePanel() {
  const { t } = useTranslation()
  const status = useStorage()
  return (
    <Panel title={t('system.storage.lifecycle.title')} description={t('system.storage.lifecycle.description')} action={<ShieldQuestion className="size-5 text-muted-foreground" />}>
      <FactList facts={Object.entries(status.data?.lifecycle ?? {}).map(([name, decision]) => ({
        id: name,
        label: t(`system.storage.lifecycle.${name}`, { defaultValue: name }),
        value: decision === 'unsupported' ? t('system.storage.lifecycle.unsupported') : t('system.storage.lifecycle.supported'),
      }))} />
    </Panel>
  )
}

/// One tier in one line: what it is, where it is, how full it is, and what the
/// last check recorded. An absent tier says so rather than showing blanks.
function tierSummary(tier: StorageTier, t: ReturnType<typeof useTranslation>['t']) {
  if (!tier.present) return t('system.storage.tiers.absent')
  const parts: string[] = []
  if (tier.mounted && tier.mount) {
    parts.push(tier.readOnly ? `${tier.mount} (${t('system.storage.tiers.readOnly')})` : tier.mount)
  } else {
    parts.push(t('system.storage.tiers.unmounted'))
  }
  if (tier.space) {
    parts.push(t('system.storage.tiers.used', { used: formatBytes(tier.space.usedBytes), total: formatBytes(tier.space.totalBytes) }))
    if (tier.space.reservedBytes > 0) {
      parts.push(t('system.storage.tiers.reserved', { reserved: formatBytes(tier.space.reservedBytes) }))
    }
  } else if (tier.partitionBytes) {
    parts.push(formatBytes(tier.partitionBytes))
  }
  if (tier.pressure) parts.push(t(`system.storage.pressure.${tier.pressure}`))
  parts.push(checkSummary(tier, t))
  return parts.join(' · ')
}

/// fsck exit 1 means errors were CORRECTED, which is the one outcome an
/// operator has to see; it gets its own sentence rather than a number.
function checkSummary(tier: StorageTier, t: ReturnType<typeof useTranslation>['t']) {
  if (tier.check.recorded === false || !tier.check.unit) return t('system.storage.tiers.neverChecked')
  if (tier.check.exitStatus === 1) return t('system.storage.tiers.checkedCorrected')
  return t('system.storage.tiers.checked', {
    result: tier.check.result ?? t('common.states.unknown'),
    status: tier.check.exitStatus ?? t('common.states.unknown'),
  })
}

/// One bind namespace in one line: who owns it, whether it is really backed
/// by DATA, and what the readiness probe did. A bind that is not on DATA is a
/// named error, never "ok" — a writer must not fall back to another
/// filesystem, and the operator has to be able to see that state. No capacity
/// is shown here: the pool is stated once, on the DATA tier.
function bindSummary(bind: StorageBind, t: ReturnType<typeof useTranslation>['t']) {
  const parts = [t(`system.storage.namespaces.owner.${bind.owner}`, { defaultValue: bind.owner })]
  parts.push(t(`system.storage.namespaces.readiness.${bind.readiness}`))
  if (bind.mounted && bind.sourceOnData === false) {
    parts.push(t('system.storage.namespaces.notOnData', { source: bind.source }))
  }
  if (bind.sourceIsDirectory === false) {
    parts.push(t('system.storage.namespaces.sourceNotDirectory', { source: bind.source }))
  }
  if (bind.readOnly) parts.push(t('system.storage.tiers.readOnly'))
  if (bind.probe) {
    if (!bind.probe.attempted) parts.push(t('system.storage.namespaces.probeSkipped', { reason: bind.probe.reason }))
    else if (bind.probe.passed) parts.push(t('system.storage.namespaces.probePassed'))
    else parts.push(t('system.storage.namespaces.probeFailed', { error: bind.probe.error }))
  }
  return parts.join(' · ')
}

/// Wear, or the reason there is none. The JEDEC estimate is a bucket range and
/// stays one: averaging it into a single percentage would invent precision the
/// device never reported. An unsupported metric is stated, never left blank.
function mediumHealth(medium: StorageMedium, t: ReturnType<typeof useTranslation>['t']) {
  if (!medium.health.supported) return t('system.storage.media.unsupported', { reason: medium.health.reason })
  const estimate = medium.health.lifetimeEstimates.find((entry) => entry.usedPercentMax !== undefined)
  const parts: string[] = []
  if (estimate) {
    parts.push(t('system.storage.media.lifetime', { min: estimate.usedPercentMin, max: estimate.usedPercentMax }))
  }
  if (medium.health.preEol) parts.push(t('system.storage.media.preEol', { state: medium.health.preEol }))
  return parts.length > 0 ? parts.join(' · ') : t('system.storage.media.unsupported', { reason: medium.health.source })
}
