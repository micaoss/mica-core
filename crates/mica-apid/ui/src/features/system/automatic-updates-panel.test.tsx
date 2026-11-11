import { cleanup, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { jsonResponse, renderPanel, stubFetch } from '@/shared/testing/panel'
import { AutomaticUpdatesPanel } from './automatic-updates-panel'

afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

/// A device following the image: nothing in `operator`.
const bakedOnly = {
  baked: { update: { source: 'https://updates.mica.example/repo', policy: 'check' } },
  operator: {},
  effective: { update: { source: 'https://updates.mica.example/repo', policy: 'check' } },
}

/// The same device re-pointed at another server.
const repointed = {
  baked: { update: { source: 'https://updates.mica.example/repo', policy: 'check' } },
  operator: { update: { source: 'https://mirror.site.example/repo' } },
  effective: { update: { source: 'https://mirror.site.example/repo', policy: 'check' } },
}

const policy = {
  lifecycle: {
    policy: {
      policy: 'check',
      checkIntervalMinutes: 1440,
      sourceUrl: 'https://updates.mica.example/repo',
      rebootPolicy: 'manual',
      maintenanceWindows: [],
    },
  },
}

describe('the automatic update policy', () => {
  /// The gate.
  it('shows a re-pointed device which address it is using and that it is not the baked one', async () => {
    stubFetch({ '/api/v1/update': policy, '/api/v1/provisioning/status': repointed })
    renderPanel(<AutomaticUpdatesPanel />)

    expect(await screen.findByText('https://mirror.site.example/repo')).toBeTruthy()
    expect(screen.getByText('set here — the image says https://updates.mica.example/repo')).toBeTruthy()
  })

  it('says a device that never overrode the address is following its image', async () => {
    stubFetch({ '/api/v1/update': policy, '/api/v1/provisioning/status': bakedOnly })
    renderPanel(<AutomaticUpdatesPanel />)

    // The address and the core channel, each following the image.
    expect(await screen.findAllByText('from the image')).toHaveLength(2)
    // The form holds the OPERATOR's value, which is empty here. Seeding it
    // from the effective value would let a save pin the image's default into
    // the operator's layer without anyone asking for it.
    expect((screen.getByLabelText(/Update server address/) as HTMLInputElement).value).toBe('')
  })

  /// An operator who cleared the address stored `null`: the device follows
  /// its image again, and must say so rather than "set here".
  it('says a device whose operator cleared the address is following its image', async () => {
    stubFetch({
      '/api/v1/update': policy,
      '/api/v1/provisioning/status': { ...bakedOnly, operator: { update: { source: null } } },
    })
    renderPanel(<AutomaticUpdatesPanel />)

    // The effective address is on screen once the layers have loaded.
    expect(await screen.findByText('https://updates.mica.example/repo')).toBeTruthy()
    expect(screen.getAllByText('from the image')).toHaveLength(2)
    expect(screen.queryByText(/set here/)).toBeNull()
  })

  /// The collapsed selects show the label, not the protocol value.
  it('shows the mode, the reboot policy and the cadence unit by their labels', async () => {
    stubFetch({
      '/api/v1/update': { lifecycle: { policy: { ...policy.lifecycle.policy, policy: 'auto', rebootPolicy: 'window' } } },
      '/api/v1/provisioning/status': bakedOnly,
    })
    renderPanel(<AutomaticUpdatesPanel />)

    const mode = await screen.findByRole('combobox', { name: 'Automatic behaviour' })
    await waitFor(() => expect(mode.textContent).toContain('Check, download and install'))
    expect(screen.getByRole('combobox', { name: 'After installing' }).textContent).toContain('Reboot in the window')
    expect(screen.getByRole('combobox', { name: 'Cadence unit' }).textContent).toContain('Days')
  })

  /// A value this console does not know is shown as such, never as another one.
  it('shows a mode it does not know as unknown, with the value', async () => {
    stubFetch({
      '/api/v1/update': { lifecycle: { policy: { ...policy.lifecycle.policy, policy: 'download' } } },
      '/api/v1/provisioning/status': bakedOnly,
    })
    renderPanel(<AutomaticUpdatesPanel />)

    const mode = await screen.findByRole('combobox', { name: 'Automatic behaviour' })
    await waitFor(() => expect(mode.textContent).toContain('Unknown (download)'))
  })

  it('writes the address as a patch that names nothing else', async () => {
    const fetch = stubFetch({
      '/api/v1/update': policy,
      '/api/v1/provisioning/status': bakedOnly,
      'POST /api/v1/update/config': () => jsonResponse({ source: { url: 'https://mirror.site.example/repo/' } }),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    await userEvent.type(await screen.findByLabelText(/Update server address/), 'https://mirror.site.example/repo/')
    await userEvent.click(screen.getByRole('button', { name: 'Save source' }))

    await waitFor(() => expect(fetch.mock.calls.some(([input]) => String(input) === '/api/v1/update/config')).toBe(true))
    const call = fetch.mock.calls.find(([input]) => String(input) === '/api/v1/update/config')
    expect(call?.[1]?.method).toBe('POST')
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({ source: { url: 'https://mirror.site.example/repo/' } })
  })

  /// The core channel is the operator's to change, and clearing it returns
  /// the device to the channel its image follows.
  it('writes the core channel the operator names, and null when it is cleared', async () => {
    const fetch = stubFetch({
      '/api/v1/update': policy,
      '/api/v1/provisioning/status': { ...bakedOnly, operator: { update: { channels: { core: 'lts' } } } },
      'POST /api/v1/update/config': () => jsonResponse({ channels: { core: 'beta' } }),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    const field = await screen.findByLabelText(/Core channel/)
    await waitFor(() => expect((field as HTMLInputElement).value).toBe('lts'))
    await userEvent.clear(field)
    await userEvent.type(field, 'beta')
    await userEvent.click(screen.getByRole('button', { name: 'Save source' }))
    const body = async (at: number) => {
      await waitFor(() => expect(fetch.mock.calls.filter(([input]) => String(input) === '/api/v1/update/config')).toHaveLength(at))
      return JSON.parse(String(fetch.mock.calls.filter(([input]) => String(input) === '/api/v1/update/config')[at - 1]?.[1]?.body))
    }
    expect((await body(1)).channels).toEqual({ core: 'beta' })

    await userEvent.clear(field)
    await userEvent.click(screen.getByRole('button', { name: 'Save source' }))
    expect((await body(2)).channels).toEqual({ core: null })
  })

  it('writes the mode, the cadence and the reboot policy together', async () => {
    const fetch = stubFetch({
      '/api/v1/update': policy,
      '/api/v1/provisioning/status': bakedOnly,
      'POST /api/v1/update/config': () => jsonResponse({ policy: 'check' }),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Save policy' }))

    const call = fetch.mock.calls.find(([input]) => String(input) === '/api/v1/update/config')
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({
      policy: 'check',
      checkIntervalMinutes: 1440,
      // No anchor on this device, and an untouched form says so rather than
      // leaving the key out: `null` is what clears one.
      checkAt: null,
      rebootPolicy: 'manual',
    })
  })

  /// 1440 is a day. An operator reading "check every 1440" has to do the
  /// division themselves, and typing a daily cadence means getting it right.
  it('reads a daily cadence back as one day rather than 1440 minutes', async () => {
    const fetch = stubFetch({
      '/api/v1/update': policy,
      '/api/v1/provisioning/status': bakedOnly,
      'POST /api/v1/update/config': () => jsonResponse({ policy: 'check' }),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    expect((await screen.findByLabelText(/Check every/) as HTMLInputElement).value).toBe('1')
    expect(screen.getByRole('combobox', { name: 'Cadence unit' })).toBeTruthy()

    // The unit it is shown in is display only: an untouched form writes back
    // the minute count the device already holds.
    await userEvent.click(screen.getByRole('button', { name: 'Save policy' }))
    const call = fetch.mock.calls.find(([input]) => String(input) === '/api/v1/update/config')
    expect(JSON.parse(String(call?.[1]?.body)).checkIntervalMinutes).toBe(1440)
  })

  /// "Check once a day at 03:00" is a time of day, not an interval from
  /// whenever the device last booted.
  it('writes a check time and clears it back to the interval', async () => {
    const anchored = {
      lifecycle: { policy: { ...policy.lifecycle.policy, checkAt: '03:00' } },
    }
    const fetch = stubFetch({
      '/api/v1/update': anchored,
      '/api/v1/provisioning/status': bakedOnly,
      'POST /api/v1/update/config': () => jsonResponse({ policy: 'check' }),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    // The device's anchor is what the field holds, once the read lands, and
    // it takes precedence: the interval is not editable under it.
    const at = await screen.findByLabelText(/Check at/)
    await waitFor(() => expect((at as HTMLInputElement).value).toBe('03:00'))
    expect((screen.getByLabelText(/Check every/) as HTMLInputElement).disabled).toBe(true)
    expect(screen.getByText(/A check time takes precedence over the interval/)).toBeTruthy()
    await userEvent.click(screen.getByRole('button', { name: 'Save policy' }))
    expect(JSON.parse(String(fetch.mock.calls.find(([input]) => String(input) === '/api/v1/update/config')?.[1]?.body)).checkAt).toBe('03:00')

    // Clearing the box is the device back on its interval, which is `null`
    // and not an empty string.
    await userEvent.clear(screen.getByLabelText(/Check at/))
    await userEvent.click(screen.getByRole('button', { name: 'Save policy' }))
    const calls = fetch.mock.calls.filter(([input]) => String(input) === '/api/v1/update/config')
    expect(JSON.parse(String(calls[calls.length - 1]?.[1]?.body))).toMatchObject({ checkAt: null, checkIntervalMinutes: 1440 })
    expect((screen.getByLabelText(/Check every/) as HTMLInputElement).disabled).toBe(false)
  })

  it('refuses a cadence that is not a whole number of minutes, and sends nothing', async () => {
    const fetch = stubFetch({
      '/api/v1/update': policy,
      '/api/v1/provisioning/status': bakedOnly,
      'POST /api/v1/update/config': () => jsonResponse({ policy: 'check' }),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    await userEvent.click(await screen.findByRole('combobox', { name: 'Cadence unit' }))
    await userEvent.click(await screen.findByRole('option', { name: 'Minutes' }))
    const every = screen.getByLabelText(/Check every/)
    await userEvent.clear(every)
    await userEvent.type(every, '0.5')
    await userEvent.click(screen.getByRole('button', { name: 'Save policy' }))

    expect(await screen.findByText('The interval must be a whole number of minutes, 0 or more.')).toBeTruthy()
    expect(fetch.mock.calls.some(([input]) => String(input) === '/api/v1/update/config')).toBe(false)
  })

  it('writes the cadence in minutes whatever unit it was entered in', async () => {
    const fetch = stubFetch({
      '/api/v1/update': policy,
      '/api/v1/provisioning/status': bakedOnly,
      'POST /api/v1/update/config': () => jsonResponse({ policy: 'check' }),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    await userEvent.click(await screen.findByRole('combobox', { name: 'Cadence unit' }))
    await userEvent.click(await screen.findByRole('option', { name: 'Hours' }))
    const every = screen.getByLabelText(/Check every/)
    await userEvent.clear(every)
    await userEvent.type(every, '6')
    await userEvent.click(screen.getByRole('button', { name: 'Save policy' }))

    const call = fetch.mock.calls.find(([input]) => String(input) === '/api/v1/update/config')
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({
      policy: 'check',
      checkIntervalMinutes: 360,
      checkAt: null,
      rebootPolicy: 'manual',
    })
  })

  it('edits the window list the device holds, by day, with an every-day shortcut', async () => {
    const twoWindows = {
      lifecycle: {
        policy: {
          ...policy.lifecycle.policy,
          maintenanceWindows: [
            { days: ['mon'], start: '02:00', end: '04:00' },
            { days: ['sat', 'sun'], start: '22:00', end: '01:00' },
          ],
        },
      },
    }
    const fetch = stubFetch({
      '/api/v1/update': twoWindows,
      '/api/v1/provisioning/status': bakedOnly,
      'POST /api/v1/update/config': () => jsonResponse({}),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    const first = await screen.findByRole('group', { name: 'Window 1' })
    const second = screen.getByRole('group', { name: 'Window 2' })
    expect(within(first).getByRole('button', { name: 'Mon' }).getAttribute('aria-pressed')).toBe('true')
    expect(within(first).getByRole('button', { name: 'Tue' }).getAttribute('aria-pressed')).toBe('false')
    // A window ending at or before its start runs into the next day, and says so.
    expect(within(second).getByText('Ends 01:00 the next day.')).toBeTruthy()
    expect(within(first).queryByText(/the next day/)).toBeNull()

    await userEvent.click(within(first).getByRole('button', { name: 'Every day' }))
    await userEvent.click(screen.getByRole('button', { name: 'Save windows' }))

    await waitFor(() => expect(fetch.mock.calls.some(([input]) => String(input) === '/api/v1/update/config')).toBe(true))
    const call = fetch.mock.calls.find(([input]) => String(input) === '/api/v1/update/config')
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({
      maintenance: {
        windows: [
          { days: ['mon', 'tue', 'wed', 'thu', 'fri', 'sat', 'sun'], start: '02:00', end: '04:00' },
          { days: ['sat', 'sun'], start: '22:00', end: '01:00' },
        ],
      },
    })
  })

  it('does not send a window that names no day', async () => {
    const oneWindow = { lifecycle: { policy: { ...policy.lifecycle.policy, maintenanceWindows: [{ days: ['mon'], start: '02:00', end: '04:00' }] } } }
    const fetch = stubFetch({
      '/api/v1/update': oneWindow,
      '/api/v1/provisioning/status': bakedOnly,
      'POST /api/v1/update/config': () => jsonResponse({}),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    await userEvent.click(within(await screen.findByRole('group', { name: 'Window 1' })).getByRole('button', { name: 'Mon' }))
    await userEvent.click(screen.getByRole('button', { name: 'Save windows' }))

    expect(await screen.findByText('Choose at least one day for each window.')).toBeTruthy()
    expect(fetch.mock.calls.some(([input]) => String(input) === '/api/v1/update/config')).toBe(false)
  })

  it('reports a refusal from the device verbatim, naming the rule it broke', async () => {
    stubFetch({
      '/api/v1/update': policy,
      '/api/v1/provisioning/status': bakedOnly,
      'POST /api/v1/update/config': () => jsonResponse({
        error: {
          code: 'validation_failed',
          message: '/mica/config/updates.json: policy `auto` requires at least one maintenance window',
        },
      }, 422),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    await userEvent.click(await screen.findByRole('button', { name: 'Save policy' }))

    expect(await screen.findByText(/requires at least one maintenance window/)).toBeTruthy()
  })

  it('names every deferral the automatic path records and shows how long it has waited', async () => {
    const expected: [string, RegExp][] = [
      ['no-newer-release', /publishes nothing newer/],
      ['outside-window', /outside the maintenance window/],
      ['clock-untrusted', /does not trust its clock/],
      ['reboot-gate-closed', /must not be interrupted/],
      // The fallback the daemon can actually produce: it clamps every
      // reason to the published set and reports anything else as `unknown`,
      // so a made-up word here would be testing a case no
      // device sends.
      ['unknown', /no name for/],
    ]
    for (const [reason, copy] of expected) {
      stubFetch({
        '/api/v1/update': {
          lifecycle: {
            policy: policy.lifecycle.policy,
            deferred: { reason, detail: 'the device said this', waitedSeconds: 7200, attempts: 5 },
          },
        },
        '/api/v1/provisioning/status': bakedOnly,
      })
      renderPanel(<AutomaticUpdatesPanel />)
      expect(await screen.findByText(copy)).toBeTruthy()
      expect(screen.getByText('the device said this')).toBeTruthy()
      expect(screen.getByText('Refused for 120 minutes, over 5 attempts.')).toBeTruthy()
      cleanup()
    }
  })

  it('reports a configuration that could not be read instead of an empty form', async () => {
    stubFetch({
      '/api/v1/update': { lifecycle: { policy_error: 'parse /mica/config/updates.json: expected value' } },
      '/api/v1/provisioning/status': () => jsonResponse({ error: { message: 'parse /mica/config/updates.json' } }, 409),
    })
    renderPanel(<AutomaticUpdatesPanel />)

    expect(await screen.findByText(/could not be read/)).toBeTruthy()
  })
})
