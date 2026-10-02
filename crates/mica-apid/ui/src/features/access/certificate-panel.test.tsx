import { cleanup, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { CertificatePanel, splitList } from './certificate-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

const certificate = {
  subject: 'CN=edge-1', issuer: 'CN=edge-1', selfSigned: true,
  dnsNames: ['edge-1.local'], ipAddresses: ['192.168.1.20'],
  notBefore: '2026-09-29T00:00:00Z', notAfter: '2028-01-01T00:00:00Z', sha256: 'ab'.repeat(32),
}

function sent(fetch: ReturnType<typeof stubFetch>, method: string) {
  const call = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === method)
  return call ? JSON.parse(String((call[1] as RequestInit).body)) : undefined
}

describe('the certificate panel', () => {
  it('describes the certificate the console serves', async () => {
    stubFetch({ '/api/v1/web/certificate': { serving: true, certificate } })
    renderPanel(<CertificatePanel />)

    expect(await screen.findByText('edge-1.local, 192.168.1.20')).toBeTruthy()
    expect(screen.getAllByText('CN=edge-1')).toHaveLength(2)
    expect(screen.getByText('self-signed')).toBeTruthy()
  })

  it('says when there is no certificate yet', async () => {
    stubFetch({ '/api/v1/web/certificate': { serving: false, certificate: null } })
    renderPanel(<CertificatePanel />)
    expect(await screen.findByText(/No certificate yet/)).toBeTruthy()
  })

  it('uploads the chain and its key as one request', async () => {
    const fetch = stubFetch({
      '/api/v1/web/certificate': { serving: true, certificate },
      'PUT /api/v1/web/certificate': () => jsonResponse(certificate),
    })
    renderPanel(<CertificatePanel />)

    await userEvent.type(await screen.findByLabelText(/Certificate \(PEM\)/), 'CHAIN')
    await userEvent.type(screen.getByLabelText(/Private key \(PEM\)/), 'KEY')
    await userEvent.click(screen.getByRole('button', { name: 'Upload certificate' }))

    await waitFor(() => expect(sent(fetch, 'PUT')).toEqual({ certificate: 'CHAIN', privateKey: 'KEY' }))
  })

  it('generates for the names given', async () => {
    const fetch = stubFetch({
      '/api/v1/web/certificate': { serving: true, certificate },
      'POST /api/v1/web/certificate/generate': () => jsonResponse(certificate),
    })
    renderPanel(<CertificatePanel />)

    const dns = await screen.findByLabelText(/DNS names/)
    await userEvent.clear(dns)
    await userEvent.type(dns, 'edge-1.local, , edge-1')
    await userEvent.click(screen.getByRole('button', { name: 'Generate certificate' }))

    await waitFor(() => expect(sent(fetch, 'POST')?.dnsNames).toEqual(['edge-1.local', 'edge-1']))
    expect(sent(fetch, 'POST')?.validityDays).toBe(825)
  })

  it('splits a comma-separated list without its blanks', () => {
    expect(splitList(' a, ,b ,')).toEqual(['a', 'b'])
  })
})
