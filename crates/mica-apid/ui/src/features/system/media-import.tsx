import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api, json } from '@/shared/lib/http'
import type { ExternalMedia } from '@/lib/types'
import { Button } from '@/shared/components/ui/button'
import { Callout } from '@/shared/components/callout'
import { RowItem, RowList } from '@/shared/components/row-item'
import { formatBytes } from '@/shared/components/fact'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { failureDetail } from '@/shared/feedback/toast'

/// One of `GET /api/v1/update/media`.
interface MediaCandidate {
  id: string
  mediaId: string
  name: string
  sizeBytes: number
  importable: boolean
  reason: string | null
}

interface MediaCandidates {
  items: MediaCandidate[]
  truncated: string[]
  unreadable: string[]
}

/// The update archives on the USB sticks and SD cards mounted now, each
/// imported on request exactly as an upload is. Listing reads names and
/// sizes only, and importing installs nothing.
export function MediaImport() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const candidates = useQuery({
    queryKey: ['update-media'],
    queryFn: () => api<MediaCandidates>('/api/v1/update/media'),
    refetchInterval: 5_000,
    retry: false,
  })
  // The volume labels, to tell two sticks apart; the storage page reads the
  // same list.
  const media = useQuery({
    queryKey: ['storage-media'],
    queryFn: () => api<ExternalMedia>('/api/v1/storage/media'),
    refetchInterval: 5_000,
    retry: false,
  })
  const importing = useMutationFeedback<unknown, MediaCandidate>({
    mutationFn: (candidate) => api<unknown>('/api/v1/update/media/import', json('POST', { mediaId: candidate.mediaId, candidateId: candidate.id })),
    success: t('system.update.media.accepted'),
    failure: t('system.update.media.import'),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: ['update-state'] }),
    // A refused import is most often a stick changed or pulled since the
    // listing, so the list is read again.
    onError: () => void queryClient.invalidateQueries({ queryKey: ['update-media'] }),
  })
  const label = (mediaId: string) => media.data?.items.find((volume) => volume.id === mediaId)?.label ?? t('system.update.media.noLabel')
  const items = candidates.data?.items ?? []
  return (
    <div className="grid gap-2 border-t pt-4">
      <h3 className="text-sm font-medium">{t('system.update.media.title')}</h3>
      <p className="text-sm text-muted-foreground">{t('system.update.media.description')}</p>
      {items.length > 0 ? (
        <RowList>
          {items.map((candidate) => (
            <RowItem
              key={candidate.id}
              title={<span className="break-all font-mono text-sm">{candidate.name}</span>}
              description={[
                label(candidate.mediaId),
                formatBytes(candidate.sizeBytes),
                candidate.importable ? undefined : t('system.update.media.tooLarge'),
              ].filter(Boolean).join(' · ')}
              actions={
                <Button
                  variant="outline"
                  size="sm"
                  disabled={!candidate.importable || importing.isPending}
                  onClick={() => importing.mutate(candidate)}
                >
                  {importing.isPending && importing.variables?.id === candidate.id ? t('system.update.media.importing') : t('system.update.media.import')}
                </Button>
              }
            />
          ))}
        </RowList>
      ) : null}
      {candidates.data && items.length === 0 ? <Callout title={t('system.update.media.none')} /> : null}
      {(candidates.data?.truncated.length ?? 0) > 0 ? <Callout tone="warning" title={t('system.update.media.truncated')} /> : null}
      {(candidates.data?.unreadable.length ?? 0) > 0 ? <Callout tone="warning" title={t('system.update.media.unreadable')} /> : null}
      {candidates.error ? <Callout tone="danger" title={failureDetail(candidates.error, t('common.requestFailed'))} /> : null}
    </div>
  )
}
