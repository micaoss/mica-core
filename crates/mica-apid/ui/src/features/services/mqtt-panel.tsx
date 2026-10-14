import { useState, type FormEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api, json } from '@/shared/lib/http'
import type { MqttConfiguration, MqttOverview, TaskAccepted } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { FactList } from '@/shared/components/fact-list'
import { FormField } from '@/shared/components/form-field'
import { Panel } from '@/shared/components/panel'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Spinner } from '@/shared/components/ui/spinner'
import { Switch } from '@/shared/components/ui/switch'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'
import { formatKnownState } from '@/i18n/format'

/// Whether a bind address reaches this device only from itself. Anything else
/// is a listener something off-host can open a connection to.
function isLoopback(address: string) {
  return address === '127.0.0.1' || address === '::1' || address.startsWith('127.')
}

/// The unit's `activeState`, found by name in the reconciler's own list.
function unitState(overview: MqttOverview | undefined, unit: string) {
  return overview?.observed.state?.units?.find((entry) => entry.unit === unit)?.activeState
}

/// The broker and the bridge: the declared listener, written as one document,
/// and what the `mqtt` reconciler reports it did with it.
///
/// The two are shown apart rather than merged. A listener that was configured
/// and a listener the broker is bound to differ for as long as the reconcile
/// takes, and after a failed one they differ until someone looks.
export function MqttPanel() {
  const { t } = useTranslation()
  const queryClient = useQueryClient()
  const mqtt = useQuery({ queryKey: ['mqtt'], queryFn: () => api<MqttOverview>('/api/v1/mqtt') })
  const [address, setAddress] = useState<string>()
  const [port, setPort] = useState<string>()
  const [auth, setAuth] = useState<boolean>()

  const configured = mqtt.data?.configured
  const currentAddress = address ?? configured?.listen.address ?? ''
  const currentPort = port ?? (configured?.listen.port ?? 1883).toString()
  const currentAuth = auth ?? configured?.auth.enabled ?? false

  const write = useMutationFeedback<TaskAccepted, MqttConfiguration>({
    mutationFn: (body) => api<TaskAccepted>('/api/v1/mqtt', json('PUT', body)),
    success: t('services.mqtt.saved'),
    failure: t('services.mqtt.save'),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['mqtt'] })
      void queryClient.invalidateQueries({ queryKey: ['settings', 'mqtt.enabled'] })
    },
  })

  // The switch above this panel owns `enabled`; this form carries it through
  // unchanged so a listener edit cannot start or stop the broker by accident.
  const submit = (event: FormEvent) => {
    event.preventDefault()
    if (!configured) return
    write.mutate({
      enabled: configured.enabled,
      listen: { address: currentAddress.trim(), port: Number(currentPort) },
      auth: { enabled: currentAuth },
    })
  }

  const observed = mqtt.data?.observed
  const running = unitState(mqtt.data, 'mica-mqtt-broker.service') === 'active'
  const openToTheNetwork = configured?.enabled === true
    && !isLoopback(configured.listen.address)
    && configured.auth.enabled === false

  return (
    <div className="grid gap-3">
      <Panel title={t('services.mqtt.listenerTitle')} description={t('services.mqtt.listenerDescription')}>
        {openToTheNetwork ? <Callout tone="warning" title={t('services.mqtt.openListener')} /> : null}
        <form className="grid gap-4" onSubmit={submit}>
          <div className="grid gap-4 sm:grid-cols-2">
            <FormField label={t('services.mqtt.listen')} hint={t('services.mqtt.listenHint')}>
              {(id) => <Input id={id} className="font-mono" value={currentAddress} onChange={(event) => setAddress(event.target.value)} placeholder="127.0.0.1" required />}
            </FormField>
            <FormField label={t('services.mqtt.port')} hint={t('services.mqtt.portHint')}>
              {(id) => <Input id={id} type="number" min={1} max={65535} value={currentPort} onChange={(event) => setPort(event.target.value)} required />}
            </FormField>
            <FormField className="sm:col-span-2" label={t('services.mqtt.auth')} hint={t('services.mqtt.authHint')}>
              {(id) => <Switch id={id} checked={currentAuth} onCheckedChange={(value) => setAuth(value)} aria-label={t('services.mqtt.auth')} />}
            </FormField>
          </div>
          {mqtt.error ? <Callout tone="danger" title={failureDetail(mqtt.error, t('common.requestFailed'))} /> : null}
          <Button className="justify-self-end" type="submit" disabled={write.isPending || !configured}>
            {write.isPending ? <Spinner /> : null}{t('services.mqtt.save')}
          </Button>
        </form>
      </Panel>

      <Panel title={t('services.mqtt.statusTitle')} description={t('services.mqtt.statusDescription')}>
        {observed?.available === false ? <Callout tone="neutral" title={t('services.mqtt.noState')} /> : null}
        <FactList facts={[
          {
            id: 'broker',
            label: t('services.mqtt.broker'),
            value: unitState(mqtt.data, 'mica-mqtt-broker.service')
              ? formatKnownState(unitState(mqtt.data, 'mica-mqtt-broker.service') as string, t)
              : t('common.notAvailable'),
          },
          {
            id: 'bridge',
            label: t('services.mqtt.bridge'),
            value: unitState(mqtt.data, 'mica-mqttd.service')
              ? formatKnownState(unitState(mqtt.data, 'mica-mqttd.service') as string, t)
              : t('common.notAvailable'),
          },
          {
            id: 'bound',
            label: t('services.mqtt.bound'),
            // Only while the broker is actually running: a listener from the
            // last published state describes something that has stopped.
            value: running && observed?.state?.listen
              ? <span className="font-mono">{observed.state.listen.address}:{observed.state.listen.port}</span>
              : t('common.notAvailable'),
          },
          {
            id: 'configPath',
            label: t('services.mqtt.configPath'),
            value: observed?.state?.configPath
              ? <span className="font-mono text-[0.8125rem] break-all">{observed.state.configPath}</span>
              : t('common.notAvailable'),
          },
        ]} />
      </Panel>
    </div>
  )
}
