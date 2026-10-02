import { useState, type FormEvent } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api, json } from '@/shared/lib/http'
import type { CertificateInfo, CertificateStatus } from '@/lib/types'
import { Callout } from '@/shared/components/callout'
import { FactList } from '@/shared/components/fact-list'
import { FilePicker } from '@/shared/components/file-picker'
import { FormField } from '@/shared/components/form-field'
import { Panel } from '@/shared/components/panel'
import { Button } from '@/shared/components/ui/button'
import { Input } from '@/shared/components/ui/input'
import { Spinner } from '@/shared/components/ui/spinner'
import { Textarea } from '@/shared/components/ui/textarea'
import { failureDetail } from '@/shared/feedback/toast'
import { useMutationFeedback } from '@/shared/feedback/use-mutation-feedback'

/// A comma-separated list, without the blanks.
export function splitList(value: string) {
  return value.split(',').map((item) => item.trim()).filter((item) => item !== '')
}

/// The console's HTTPS certificate: what it is, an upload, and a generator.
export function CertificatePanel() {
  const { t } = useTranslation()
  const status = useQuery({ queryKey: ['web-certificate'], queryFn: () => api<CertificateStatus>('/api/v1/web/certificate') })
  const certificate = status.data?.certificate

  return (
    <Panel title={t('access.certificate.title')} description={t('access.certificate.description')}>
      <div className="grid gap-6">
        {status.error ? <Callout tone="danger" title={failureDetail(status.error, t('common.requestFailed'))} /> : null}
        {status.data && !certificate ? <Callout tone="neutral" title={t('access.certificate.none')} /> : null}
        {certificate && status.data?.serving === false ? <Callout tone="neutral" title={t('access.certificate.notServing')} /> : null}
        {certificate ? <CertificateFacts certificate={certificate} /> : null}
        <UploadForm />
        <GenerateForm />
      </div>
    </Panel>
  )
}

function CertificateFacts({ certificate }: { certificate: CertificateInfo }) {
  const { t } = useTranslation()
  const names = [...certificate.dnsNames, ...certificate.ipAddresses]
  return (
    <FactList facts={[
      { id: 'subject', label: t('access.certificate.subject'), value: <span className="font-mono break-all">{certificate.subject}</span> },
      { id: 'issuer', label: t('access.certificate.issuer'), value: <span className="font-mono break-all">{certificate.issuer}</span> },
      { id: 'kind', label: t('access.certificate.kind'), value: certificate.selfSigned ? t('access.certificate.selfSigned') : t('access.certificate.issued') },
      { id: 'names', label: t('access.certificate.names'), value: <span className="font-mono break-all">{names.join(', ') || '—'}</span> },
      { id: 'validity', label: t('access.certificate.validity'), value: `${certificate.notBefore} — ${certificate.notAfter}` },
      { id: 'sha256', label: t('access.certificate.fingerprint'), value: <span className="font-mono text-[0.8125rem] break-all">{certificate.sha256}</span> },
    ]} />
  )
}

/// Replace the query's certificate with the one a write answered.
function useCertificateWrite<TBody>(path: string, method: 'PUT' | 'POST', success: string, failure: string, onDone: () => void) {
  const queryClient = useQueryClient()
  return useMutationFeedback<CertificateInfo, TBody>({
    mutationFn: (body) => api<CertificateInfo>(path, json(method, body)),
    success,
    failure,
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ['web-certificate'] })
      onDone()
    },
  })
}

/// A PEM text field that can also be filled from a file.
function PemField({ label, hint, value, onChange }: { label: string; hint: string; value: string; onChange: (value: string) => void }) {
  const { t } = useTranslation()
  return (
    <div className="grid gap-2">
      <FormField label={label} hint={hint}>
        {(id) => <Textarea id={id} className="min-h-28 font-mono text-[0.8125rem]" value={value} onChange={(event) => onChange(event.target.value)} spellCheck={false} required />}
      </FormField>
      <FilePicker label={t('access.certificate.fromFile')} accept=".pem,.crt,.cer,.key"
        chooseLabel={t('access.certificate.choose')} emptyLabel={t('access.certificate.noFile')}
        submitLabel={t('access.certificate.read')} pendingLabel={t('access.certificate.read')}
        onSubmit={(file) => void file.text().then(onChange)} />
    </div>
  )
}

function UploadForm() {
  const { t } = useTranslation()
  const [chain, setChain] = useState('')
  const [key, setKey] = useState('')
  const upload = useCertificateWrite<{ certificate: string; privateKey: string }>(
    '/api/v1/web/certificate', 'PUT', t('access.certificate.uploaded'), t('access.certificate.upload'),
    () => { setChain(''); setKey('') },
  )
  const submit = (event: FormEvent) => {
    event.preventDefault()
    upload.mutate({ certificate: chain, privateKey: key })
  }
  return (
    <form className="grid gap-4" onSubmit={submit}>
      <h3 className="text-sm font-medium">{t('access.certificate.uploadTitle')}</h3>
      <div className="grid gap-4 lg:grid-cols-2">
        <PemField label={t('access.certificate.chain')} hint={t('access.certificate.chainHint')} value={chain} onChange={setChain} />
        <PemField label={t('access.certificate.key')} hint={t('access.certificate.keyHint')} value={key} onChange={setKey} />
      </div>
      <Button className="justify-self-end" type="submit" disabled={upload.isPending || !chain || !key}>
        {upload.isPending ? <Spinner /> : null}{t('access.certificate.upload')}
      </Button>
    </form>
  )
}

function GenerateForm() {
  const { t } = useTranslation()
  const host = window.location.hostname
  const hostIsAddress = /^[\d.]+$/.test(host) || host.includes(':')
  const [commonName, setCommonName] = useState(hostIsAddress ? 'mica' : host)
  const [dnsNames, setDnsNames] = useState(hostIsAddress ? 'mica, localhost' : `${host}, localhost`)
  const [ipAddresses, setIpAddresses] = useState(hostIsAddress ? `${host}, 127.0.0.1` : '127.0.0.1')
  const [days, setDays] = useState('825')
  const generate = useCertificateWrite<{ commonName: string; dnsNames: string[]; ipAddresses: string[]; validityDays: number }>(
    '/api/v1/web/certificate/generate', 'POST', t('access.certificate.generated'), t('access.certificate.generate'), () => {},
  )
  const submit = (event: FormEvent) => {
    event.preventDefault()
    generate.mutate({ commonName: commonName.trim(), dnsNames: splitList(dnsNames), ipAddresses: splitList(ipAddresses), validityDays: Number(days) })
  }
  return (
    <form className="grid gap-4" onSubmit={submit}>
      <h3 className="text-sm font-medium">{t('access.certificate.generateTitle')}</h3>
      <div className="grid gap-4 sm:grid-cols-2">
        <FormField label={t('access.certificate.commonName')}>
          {(id) => <Input id={id} value={commonName} onChange={(event) => setCommonName(event.target.value)} required />}
        </FormField>
        <FormField label={t('access.certificate.days')} hint={t('access.certificate.daysHint')}>
          {(id) => <Input id={id} type="number" min={1} max={3650} value={days} onChange={(event) => setDays(event.target.value)} required />}
        </FormField>
        <FormField label={t('access.certificate.dnsNames')} hint={t('access.certificate.dnsNamesHint')}>
          {(id) => <Input id={id} className="font-mono" value={dnsNames} onChange={(event) => setDnsNames(event.target.value)} />}
        </FormField>
        <FormField label={t('access.certificate.ipAddresses')} hint={t('access.certificate.ipAddressesHint')}>
          {(id) => <Input id={id} className="font-mono" value={ipAddresses} onChange={(event) => setIpAddresses(event.target.value)} />}
        </FormField>
      </div>
      <Button className="justify-self-end" type="submit" disabled={generate.isPending}>
        {generate.isPending ? <Spinner /> : null}{t('access.certificate.generate')}
      </Button>
    </form>
  )
}
