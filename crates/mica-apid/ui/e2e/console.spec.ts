import { expect, test, type Page } from '@playwright/test'

test.beforeEach(async ({ page }) => {
  await mockDeviceApi(page)
})

test('navigates the complete desktop console', async ({ page, isMobile }) => {
  test.skip(isMobile, 'desktop navigation contract')
  await page.goto('./')

  await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible()
  await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toBeVisible()
  for (const name of ['Network', 'Services', 'Access', 'System']) {
    // Exact: the overview's uplink card is a link to the network page too.
    await page.getByRole('link', { name, exact: true }).click()
    await expect(page.getByRole('heading', { name, exact: true })).toBeVisible()
  }
  await expect(page.getByRole('link', { name: 'Applications' })).toHaveCount(0)
})

test('routes the reported update from the overview attention list', async ({ page, isMobile }) => {
  test.skip(isMobile, 'desktop navigation contract')
  await page.goto('./')

  await expect(page.getByText('System update 2026.09.0 is ready to install')).toBeVisible()
  await page.getByRole('link', { name: 'Open Update' }).click()
  await expect(page.getByRole('heading', { name: 'System' })).toBeVisible()
  await expect(page.getByRole('tab', { name: 'Update & recovery', selected: true })).toBeVisible()
})

test('writes real time settings', async ({ page, isMobile }) => {
  test.skip(isMobile, 'desktop system workflow')
  await page.goto('./system')
  await page.getByRole('tab', { name: 'Time' }).click()

  await expect(page.getByRole('heading', { name: 'System' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'NTP servers' })).toBeVisible()
  await expect(page.getByText('the kernel reports a bounded clock error')).toBeVisible()
  await page.getByRole('textbox', { name: /^Timezone/ }).fill('Europe/Berlin')
  await page.getByRole('button', { name: 'Save timezone' }).click()
  await expect(page.getByText('Change applied.')).toBeVisible()
})

test('summarises the network on its overview', async ({ page, isMobile }) => {
  await mockNetworkBench(page)
  test.skip(isMobile, 'desktop network workflow')
  await page.goto('./network?tab=overview')

  await expect(page.getByText('Online')).toBeVisible()
  await expect(page.getByText('Uplink eth0')).toBeVisible()
  await expect(page.getByText('via 192.168.1.1')).toBeVisible()
})

test('restores Chinese and dark appearance preferences', async ({ page, isMobile }) => {
  test.skip(isMobile, 'desktop appearance contract')
  await page.addInitScript(() => {
    localStorage.setItem('mica.ui.locale', 'zh-CN')
    localStorage.setItem('mica.ui.theme', 'dark')
  })
  await page.goto('./')

  await expect(page.getByRole('heading', { name: '概览' })).toBeVisible()
  await expect(page.locator('html')).toHaveClass(/dark/)
  await expect(page.locator('html')).toHaveAttribute('lang', 'zh-CN')
})

test('stages an interface edit and applies it through the review dialog', async ({ page, isMobile }) => {
  await mockNetworkBench(page)
  test.skip(isMobile, 'desktop network editor contract')
  await page.goto('./network')
  await page.getByRole('link', { name: 'eth0' }).click()

  await expect(page.getByRole('heading', { name: 'eth0' })).toBeVisible()
  await page.getByRole('button', { name: 'Edit' }).click()
  await page.getByRole('button', { name: 'Static' }).click()
  await page.getByRole('textbox', { name: 'Address' }).fill('192.168.1.24/24')
  await page.getByRole('button', { name: 'Done' }).click()
  await page.getByRole('button', { name: 'Review and apply' }).click()

  const dialog = page.getByRole('dialog', { name: 'Review network changes' })
  await expect(dialog).toContainText('This browser reached the device on eth0')
  const apply = dialog.getByRole('button', { name: 'Apply 1 change' })
  await expect(apply).toBeDisabled()
  await dialog.getByRole('switch').click()
  const request = page.waitForRequest((candidate) => candidate.method() === 'PUT')
  await apply.click()
  const write = await request
  expect(new URL(write.url()).pathname).toBe('/api/v1/network')
  expect(write.postDataJSON()).toMatchObject({ eth0: { dhcp: false, static: { address: '192.168.1.24/24' } }, wg0: { kind: 'wireguard' } })
  await expect(page.getByText('The device reports the interfaces reconciled.')).toBeVisible()
})

test('shows the new WireGuard public key after rotation', async ({ page }, testInfo) => {
  await mockNetworkBench(page)
  test.skip(testInfo.project.name !== 'chromium', 'one browser covers the API response contract')
  await page.goto('./network?tab=wireguard&tunnel=wg0')
  await page.getByRole('button', { name: 'Rotate key' }).click()
  await page.getByRole('alertdialog').getByRole('button', { name: 'Rotate key' }).click()

  await expect(page.getByText('rotated-e2e-public-key=').first()).toBeVisible()
})

test('captures each network page for review', async ({ page }, testInfo) => {
  await mockNetworkBench(page)
  test.skip(testInfo.project.name !== 'chromium', 'one browser is enough for review captures')
  await page.setViewportSize({ width: 1440, height: 1000 })
  for (const [name, path] of [
    ['overview', './network?tab=overview'],
    ['interfaces', './network'],
    ['interface-eth1', './network/eth1'],
    ['wifi', './network?tab=wifi'],
    ['dns', './network?tab=dns'],
    ['wireguard', './network?tab=wireguard&tunnel=wg0'],
  ] as const) {
    await page.goto(path)
    await expect(page.getByRole('heading', { name: /Network|eth1/ }).first()).toBeVisible()
    await page.waitForLoadState('networkidle')
    await page.screenshot({ path: testInfo.outputPath(`network-${name}.png`), fullPage: true })
  }
  await page.goto('./network/eth1')
  await page.getByRole('button', { name: 'Edit' }).click()
  await page.getByRole('textbox', { name: 'Pool start' }).fill('192.168.10.50')
  await page.getByRole('button', { name: 'Done' }).click()
  await page.getByRole('button', { name: 'Review and apply' }).click()
  const review = page.getByRole('dialog', { name: 'Review network changes' })
  await expect(review).toBeVisible()
  // The dialog fades in; capture it at rest.
  await expect(review).toHaveCSS('opacity', '1')
  await page.screenshot({ path: testInfo.outputPath('network-review.png') })

  await page.route('**/api/v1/dns/test', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify([{ server: '9.9.9.9', reachable: true, latencyMs: 18 }, { server: '149.112.112.112', reachable: false, latencyMs: null }]) }))
  await page.goto('./network?tab=dns')
  await page.getByRole('button', { name: 'Edit' }).first().click()
  await page.getByRole('button', { name: 'Test servers' }).click()
  await expect(page.getByText('Reachable · 18 ms')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('network-dns-edit.png'), fullPage: true })

  await page.goto('./network?tab=wifi')
  await page.getByRole('button', { name: 'Edit' }).last().click()
  await page.getByRole('button', { name: 'Access point' }).click()
  await page.mouse.wheel(0, 4000)
  await page.screenshot({ path: testInfo.outputPath('network-wifi-ap-edit.png') })

  await page.goto('./network?tab=wireguard&tunnel=wg0')
  await page.getByRole('button', { name: 'Add peer' }).click()
  const peer = page.getByRole('dialog', { name: 'Add peer' })
  await expect(peer).toHaveCSS('opacity', '1')
  await page.screenshot({ path: testInfo.outputPath('network-peer-add.png') })
})

test('uses the mobile navigation drawer', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'mobile navigation contract')
  await page.goto('./')

  await page.getByRole('button', { name: 'Open navigation' }).click()
  await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toBeVisible()
  await page.getByRole('link', { name: 'Network' }).click()
  await expect(page.getByRole('heading', { name: 'Network' })).toBeVisible()
})

test('keeps a tall dialog inside a short appliance screen', async ({ page }, testInfo) => {
  await mockNetworkBench(page)
  test.skip(testInfo.project.name !== 'chromium', 'one browser covers the geometry')
  // A 1024x600 panel is a normal appliance display. The dialog is height
  // bounded and scrolls inside, so at 520px its title and both footer buttons
  // stay on screen.
  await page.setViewportSize({ width: 1024, height: 520 })
  await page.goto('./network')
  await page.getByRole('button', { name: 'Add', exact: true }).click()

  const dialog = page.locator('[data-slot=dialog-content]')
  const box = (await dialog.boundingBox())!
  expect(box.y).toBeGreaterThanOrEqual(0)
  expect(box.y + box.height).toBeLessThanOrEqual(520)
  await expect(page.getByRole('heading', { name: 'Add an interface' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Stage' })).toBeVisible()
  await page.getByRole('button', { name: 'Cancel' }).click()
  await expect(dialog).toHaveCount(0)
})

test('opens the language picker without a second layer behind it', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'one browser covers the overlay contract')
  // The picker is not a dialog inside the header menu, so no menu is left open
  // above its own backdrop.
  await page.goto('./')
  await page.getByRole('combobox', { name: 'Language' }).click()

  await expect(page.getByRole('option', { name: /简体中文/ })).toBeVisible()
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect(page.locator('[data-slot=dropdown-menu-popup]')).toHaveCount(0)
})

test('confirms a destructive action by closing it and saying what happened', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'one browser covers the confirmation contract')
  // Ten of the eleven confirmations left the modal open with nothing said, so
  // a completed action and a lost click looked identical.
  await page.goto('./system')
  await page.getByRole('button', { name: 'Reboot' }).click()
  const dialog = page.getByRole('alertdialog')
  await expect(dialog).toBeVisible()
  await dialog.getByRole('button', { name: 'Reboot' }).click()

  await expect(dialog).toHaveCount(0)
  await expect(page.getByText('Power action accepted.').first()).toBeVisible()
})

test('reports a saved policy', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'one browser covers the feedback contract')
  await page.goto('./system')
  await page.getByRole('tab', { name: /Update/ }).click()
  await page.getByRole('button', { name: 'Save policy' }).click()

  await expect(page.getByText('Update policy saved.').first()).toBeVisible()
})

test('matches the approved responsive visual baseline', async ({ page }, testInfo) => {
  if (testInfo.project.name === 'tablet') {
    await page.addInitScript(() => localStorage.setItem('mica.ui.theme', 'dark'))
    await page.goto('./services')
    await expect(page.locator('html')).toHaveClass(/dark/)
    await expect(page).toHaveScreenshot('services-tablet-dark.png', { animations: 'disabled', fullPage: true })
    return
  }

  await page.goto('./')
  if (testInfo.project.name === 'mobile') {
    await page.getByRole('button', { name: 'Open navigation' }).click()
    await expect(page).toHaveScreenshot('overview-mobile-drawer-light.png', { animations: 'disabled', fullPage: true })
    return
  }
  await expect(page).toHaveScreenshot('overview-desktop-light.png', { animations: 'disabled', fullPage: true })
})

async function mockDeviceApi(page: Page) {
  await page.route('**/api/v1/**', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    const body = payload(path, request.method())
    await route.fulfill({
      status: body === undefined ? 204 : 200,
      contentType: body === undefined ? undefined : 'application/json',
      body: body === undefined ? undefined : JSON.stringify(body),
    })
  })
}

const FIVE_MINUTES_AGO = () => new Date(Date.now() - 5 * 60_000).toISOString()

function payload(path: string, method: string): unknown {
  if (method !== 'GET') {
    if (path === '/api/v1/ui/active') return { mode: 'builtIn' }
    if (path === '/api/v1/actions/wireguard/wg0/rotate-key') return { publicKey: 'rotated-e2e-public-key=' }
    return { taskId: 'task-e2e' }
  }
  if (path === '/api/v1/session') return { state: 'authenticated', csrfToken: 'e2e-csrf', localAddress: '192.168.1.24' }
  if (path === '/api/v1/health') return { apid: 'ok', micad: 'ok', checkedAt: 183900 }
  if (path === '/api/v1/meta') return { api: 'v1', settingsSchemaVersion: 1, daemon: 'micad 0.0.1', init: 'systemd' }
  if (path === '/api/v1/system/info') return {
    machineId: { available: true, id: '4f2e9c1a7b3d4e5f' },
    board: { available: true, model: 'mica-cm4 rev 2', source: 'device-tree' },
    kernel: { available: true, release: '6.6.52-mica', version: '#1 SMP' },
    release: { available: true, name: 'mica', versionId: '2026.08.2', imageVersion: '2026.08.2', prettyName: 'mica 2026.08.2' },
    system: { available: true, version: '2026.08.2', package: 'mica-system', buildDate: '2026-08-19' },
    daemon: { available: true, name: 'micad', version: '0.0.1' },
    packages: { available: true, count: 214, micaCount: 12, entries: [] },
    slot: { available: true, booted: 'A', bootname: 'rootfs.0', bootStatus: 'good', primary: true },
    uptime: { available: true, seconds: 1231932 },
  }
  if (path === '/api/v1/settings/hostname') return 'mica-cm4'
  if (path === '/api/v1/network') return {
    configured: { eth0: { dhcp: true }, wg0: { kind: 'wireguard', dhcp: false, static: { address: '10.10.0.2/24', dns: [] }, wireguard: { listenPort: 51820, peers: [] } } },
    configuredCount: 2,
    observed: { available: true, interfaceCount: 2, interfaces: [{ index: 2, name: 'eth0', kind: 'ether', operationalState: 'routable', addresses: ['192.168.1.24/24'] }, { index: 4, name: 'wg0', kind: 'wireguard', operationalState: 'routable', addresses: ['10.10.0.2/24'] }] },
  }
  if (path === '/api/v1/tasks') return [{ id: 'task-1', operation: 'set', dotPath: 'hostname', source: 'api', status: 'finished', outcome: 'succeeded', enqueuedAt: FIVE_MINUTES_AGO(), foldedCount: 0 }]
  if (path === '/api/v1/tasks/task-e2e') return { id: 'task-e2e', operation: 'set', dotPath: 'settings', source: 'api', status: 'finished', outcome: 'succeeded', enqueuedAt: FIVE_MINUTES_AGO(), foldedCount: 0 }
  if (path === '/api/v1/ui') return { mode: 'builtIn' }
  if (path === '/api/v1/settings/container.enabled' || path === '/api/v1/settings/mqtt.enabled' || path === '/api/v1/settings/access.ssh.enabled') return true
  if (path === '/api/v1/state/container') return { enabled: true, service: 'docker.service', serviceState: 'active' }
  if (path === '/api/v1/state/mqtt') return { enabled: true, listen: { address: '127.0.0.1', port: 1883 }, units: [{ unit: 'mica-mqtt-broker.service', activeState: 'active', unitFileState: 'enabled' }] }
  if (path === '/api/v1/tokens') return []
  if (path === '/api/v1/ssh/authorized-keys') return { keys: [], notice: 'Every authorized key grants root on this appliance.' }
  if (path === '/api/v1/settings/wifi.client') return { enabled: true, interface: 'wlan0', networks: [] }
  if (path === '/api/v1/wifi/client/networks' || path.endsWith('/peers')) return []
  if (path === '/api/v1/update') return { lifecycle: { state: 'ready', available: { name: 'mica', version: '2026.09.0' }, reboot_gate: { safe: true, reasons: [] }, client: { available: true } }, booted_slot: 'rootfs.0' }
  if (path === '/api/v1/settings/time.ntp.servers') return ['0.pool.ntp.org']
  if (path === '/api/v1/settings/time.timezone') return 'Etc/UTC'
  if (path === '/api/v1/time/status') return {
    status: 'synchronized',
    synchronized: true,
    server: { name: 'time.cloudflare.com', address: '162.159.200.1' },
    sample: { leap: 0, stratum: 3, spike: false, offsetSeconds: 0.0024, packetCount: 8, correction: 'slew' },
  }
  if (path === '/api/v1/network/status') return {
    interfaces: { available: true, count: 1, entries: [{
      name: 'eth0',
      link: { operationalState: 'routable', carrierState: 'carrier', carrier: true },
      addresses: [{ family: 'inet', address: '192.168.1.24', prefixLength: 24, configSource: 'DHCPv4' }],
      dhcp: { available: true, state: 'bound', lease: { server: '192.168.1.1' } },
      dns: ['192.168.1.1'],
    }] },
    defaultRoutes: { available: true, count: 1, entries: [{ family: 'inet', gateway: '192.168.1.1', interface: 'eth0', metric: 100 }] },
    dns: { available: true, linkServers: ['192.168.1.1'], resolverServers: ['127.0.0.53'], probe: { name: 'deb.debian.org', reachable: true, result: 'resolved' } },
    wifi: { available: true, associations: [] },
    capabilities: { wifi: { supported: false, interfaces: [] }, bluetooth: { supported: false, adapters: [] }, cellular: { supported: false, interfaces: [] } },
  }
  if (path === '/api/v1/ui/bundles') return { bundles: [], retentionLimit: 32 }
  return {}
}

/// A bench device for the network pages: an uplink, a statically addressed LAN,
/// an unplugged port, a joined radio and a tunnel with one peer. Registered
/// after the device mock, so its answers take precedence.
async function mockNetworkBench(page: Page) {
  await page.route('**/api/v1/**', async (route) => {
    const request = route.request()
    const body = request.method() === 'GET' ? benchPayload(new URL(request.url()).pathname) : undefined
    if (body === undefined) return route.fallback()
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) })
  })
}

const PEER = { name: 'Harbour office', publicKey: 'cmVtb3RlUGVlcktleXJlbW90ZVBlZXJLZXlyZW1vdGU=', presharedKey: '<redacted>', allowedIps: ['10.10.0.1/32'], endpoint: 'vpn.example.eu:51820', persistentKeepalive: 25 }

function benchPayload(path: string): unknown {
  if (path === '/api/v1/network') return {
    configured: {
      eth0: { dhcp: true },
      eth1: { dhcp: false, static: { address: '192.168.10.1/24' } },
      wg0: { kind: 'wireguard', dhcp: false, static: { address: '10.10.0.2/24', dns: [] }, wireguard: { listenPort: 51820, peers: [PEER] } },
    },
    configuredCount: 3,
    revision: 'e2e-revision',
    observed: { available: true, interfaceCount: 0, interfaces: [] },
  }
  if (path === '/api/v1/dns') return {
    global: { enabled: true, servers: ['9.9.9.9', '149.112.112.112'] },
    mode: 'global',
    effective: { global: ['9.9.9.9', '149.112.112.112'] },
    staticRecords: [
      { id: 'a1f3c2d4', hostname: 'nas.lan', address: '192.168.10.20', comment: 'Chart server' },
      { id: 'b7e9d0a1', hostname: 'plotter', address: '192.168.10.30' },
    ],
    probe: { ok: true, name: '0.debian.pool.ntp.org', latencyMs: 24, checkedAt: '2026-10-21T08:00:00Z' },
  }
  if (path === '/api/v1/network/wg0/peers') return [PEER]
  if (path === '/api/v1/state/network') return { wg0: { publicKey: 'e2eDevicePublicKeye2eDevicePublicKeye2eDev0=' } }
  if (path === '/api/v1/wifi/client') return { enabled: true, interface: 'wlan0' }
  if (path === '/api/v1/wifi/ap') return { mode: 'off', interface: 'wlan0', ssid: 'mica-cm4', psk: '<redacted>', channel: 6, countryCode: 'DE', address: '192.168.4.1/24', holdDownSeconds: 120, graceSeconds: 60 }
  if (path === '/api/v1/wifi/client/networks') return [
    { ssid: 'marina-guest', psk: '<redacted>', hidden: false, priority: 10 },
    { ssid: 'fuel-dock', psk: '<redacted>', hidden: false, priority: 1, autoConnect: false },
  ]
  if (path === '/api/v1/network/status') return {
    interfaces: { available: true, count: 5, entries: [
      {
        name: 'eth0', index: 2, hardwareAddress: '02:42:ac:11:00:02',
        link: { operationalState: 'routable', carrierState: 'carrier', carrier: true },
        addresses: [{ family: 'inet', address: '192.168.1.24', prefixLength: 24, scope: 'global', configSource: 'DHCPv4' }],
        dhcp: { available: true, state: 'bound', lease: { server: '192.168.1.1', router: '192.168.1.1', lifetimeSeconds: 3480 } },
        dns: ['192.168.1.1'],
      },
      {
        name: 'eth1', index: 3, hardwareAddress: '02:42:ac:11:00:03',
        link: { operationalState: 'routable', carrierState: 'carrier', carrier: true },
        addresses: [{ family: 'inet', address: '192.168.10.1', prefixLength: 24, scope: 'global', configSource: 'static' }],
        dhcp: { available: false }, dns: [],
      },
      { name: 'eth2', index: 4, link: { operationalState: 'no-carrier', carrier: false }, addresses: [], dhcp: { available: false }, dns: [] },
      {
        name: 'wlan0', index: 5, type: 'wlan',
        link: { operationalState: 'routable', carrier: true },
        addresses: [{ family: 'inet', address: '10.20.0.31', prefixLength: 24, scope: 'global', configSource: 'DHCPv4' }],
        dhcp: { available: true }, dns: [],
      },
      { name: 'wg0', index: 6, kind: 'wireguard', link: { operationalState: 'routable', carrier: true }, addresses: [{ family: 'inet', address: '10.10.0.2', prefixLength: 24, scope: 'global' }], dhcp: { available: false }, dns: [] },
    ] },
    defaultRoutes: { available: true, count: 1, entries: [{ family: 'inet', gateway: '192.168.1.1', interface: 'eth0', metric: 100 }] },
    dns: { available: true, linkServers: ['192.168.1.1'], resolverServers: ['127.0.0.53'], probe: { name: 'deb.debian.org', reachable: true, result: 'resolved' } },
    wifi: { available: true, associations: [{ interface: 'wlan0', associated: true, ssid: 'marina-guest', rssiDbm: -58 }] },
    accessPoint: { available: true, entries: [] },
    capabilities: { wifi: { supported: true, interfaces: ['wlan0'] }, bluetooth: { supported: false, adapters: [] }, cellular: { supported: false, interfaces: [] } },
  }
  return undefined
}
