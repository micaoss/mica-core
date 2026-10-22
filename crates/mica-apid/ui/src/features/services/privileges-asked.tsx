import { useTranslation } from 'react-i18next'

/// What a bundle or an update asks for beyond the installed version.
export interface PrivilegesAsked {
  root?: boolean
  capabilities?: string[]
  devices?: string[]
}

/// `asked`, a line each: root, the capabilities and the devices.
export function PrivilegesList({ asked }: { asked: PrivilegesAsked }) {
  const { t } = useTranslation()
  const list = (items?: string[]) => (items ?? []).join(', ')
  return (
    <span className="grid gap-1">
      {asked.root ? <span className="font-medium">{t('services.apps.privileges.root')}</span> : null}
      {asked.capabilities?.length ? <span className="font-mono">{t('services.apps.privileges.capabilities', { list: list(asked.capabilities) })}</span> : null}
      {asked.devices?.length ? <span className="font-mono">{t('services.apps.privileges.devices', { list: list(asked.devices) })}</span> : null}
    </span>
  )
}
