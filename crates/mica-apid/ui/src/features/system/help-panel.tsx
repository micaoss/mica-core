import { Link } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { BookOpen, Wrench } from 'lucide-react'
import { buttonVariants } from '@/shared/components/ui/button'
import { Panel } from '@/shared/components/panel'
import { SupportInfoPanel } from './support-info-panel'

/// The local guides: each names its steps and where the fix is made.
const GUIDES = [
  { id: 'updateCheck', steps: 4, link: { to: '/system', hash: 'update' } },
  { id: 'offlineImport', steps: 3, link: { to: '/system', hash: 'update' } },
  { id: 'clock', steps: 3, link: { to: '/system', hash: 'time' } },
  { id: 'wireguard', steps: 4, link: { to: '/network', search: { tab: 'wireguard' } } },
] as const

/// Help: documentation (none is configured for this version yet), the local
/// troubleshooting guides, and the support summary to send with a request.
/// Everything here works without a connection.
export function HelpPanel() {
  const { t } = useTranslation()
  return (
    <div className="grid gap-3">
      <Panel title={t('system.help.documentation.title')} action={<BookOpen className="size-5 text-muted-foreground" />}>
        <p className="text-sm text-muted-foreground">{t('system.help.documentation.none')}</p>
      </Panel>
      <Panel title={t('system.help.troubleshooting.title')} description={t('system.help.troubleshooting.description')} action={<Wrench className="size-5 text-muted-foreground" />} contentClassName="gap-5">
        {GUIDES.map((guide) => (
          <section key={guide.id} className="grid gap-2" aria-labelledby={`help-${guide.id}`}>
            <h3 id={`help-${guide.id}`} className="text-sm font-medium">{t(`system.help.guides.${guide.id}.title`)}</h3>
            <ol className="ml-5 list-decimal space-y-1 text-sm text-muted-foreground">
              {Array.from({ length: guide.steps }, (_, index) => (
                <li key={index}>{t(`system.help.guides.${guide.id}.step${index + 1}` as `system.help.guides.updateCheck.step1`)}</li>
              ))}
            </ol>
            <Link {...guide.link} className={buttonVariants({ variant: 'outline', size: 'sm', className: 'justify-self-start' })}>
              {t(`system.help.guides.${guide.id}.action`)}
            </Link>
          </section>
        ))}
        <p className="text-sm text-muted-foreground">{t('system.help.troubleshooting.stillStuck')}</p>
      </Panel>
      <SupportInfoPanel />
    </div>
  )
}
