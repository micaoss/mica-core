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
  await page.goto('./system?tab=information')
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

  await expect(page.getByText('Online', { exact: true })).toBeVisible()
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
  await page.getByRole('button', { name: 'Edit configuration' }).click()
  await page.getByRole('button', { name: 'Static' }).click()
  await page.getByRole('textbox', { name: 'Address' }).fill('192.168.1.24/24')
  await page.getByRole('button', { name: 'Done' }).click()
  await page.getByRole('button', { name: 'Review and apply' }).click()

  const dialog = page.getByRole('dialog', { name: 'Review network changes' })
  await expect(dialog).toContainText('This browser reached the device on eth0')
  const apply = dialog.getByRole('button', { name: 'Apply 1 change' })
  await expect(apply).toBeDisabled()
  await dialog.getByRole('switch').click()
  const request = page.waitForRequest((candidate) => candidate.method() === 'PUT' && !candidate.url().includes('dryRun'))
  await apply.click()
  const write = await request
  expect(new URL(write.url()).pathname).toBe('/api/v1/network')
  expect(write.postDataJSON()).toMatchObject({ baseVersion: 'e2e-version', configured: { eth0: { dhcp: false, static: { address: '192.168.1.24/24' } }, wg0: { kind: 'wireguard' } } })
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
    ['interface-eth0', './network/eth0'],
    ['interface-eth1', './network/eth1'],
    ['interface-can0', './network/can0'],
    ['wifi', './network?tab=wifi'],
    ['dns', './network?tab=dns'],
    ['wireguard', './network?tab=wireguard&tunnel=wg0'],
  ] as const) {
    await page.goto(path)
    await expect(page.getByRole('heading', { name: /Network|eth0|eth1|can0/ }).first()).toBeVisible()
    await page.waitForLoadState('networkidle')
    await page.screenshot({ path: testInfo.outputPath(`network-${name}.png`), fullPage: true })
  }
  await page.goto('./network/eth1')
  await page.getByRole('button', { name: 'Edit configuration' }).click()
  await page.getByRole('textbox', { name: 'Address', exact: true }).fill('192.168.10.2/24')
  await page.getByRole('button', { name: 'Done' }).click()
  await page.getByRole('button', { name: 'Review and apply' }).click()
  const review = page.getByRole('dialog', { name: 'Review network changes' })
  await expect(review).toBeVisible()
  // The dialog fades in; capture it at rest.
  await expect(review).toHaveCSS('opacity', '1')
  await page.screenshot({ path: testInfo.outputPath('network-review.png') })

  await page.route('**/api/v1/dns/test', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify([{ server: '9.9.9.9', reachable: true, latencyMs: 18 }, { server: '149.112.112.112', reachable: false, latencyMs: null }]) }))
  await page.goto('./network?tab=dns')
  await page.getByRole('button', { name: 'Edit configuration' }).click()
  await page.getByRole('button', { name: 'Test servers' }).click()
  await expect(page.getByText('Reachable · 18 ms')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('network-dns-edit.png'), fullPage: true })

  await page.goto('./network?tab=wifi')
  await page.getByRole('button', { name: 'Edit configuration' }).click()
  await page.getByRole('button', { name: 'Access point' }).click()
  await page.mouse.wheel(0, 4000)
  await page.screenshot({ path: testInfo.outputPath('network-wifi-ap-edit.png') })

  await page.goto('./network?tab=wireguard&tunnel=wg0')
  await page.getByRole('button', { name: 'Add peer' }).click()
  const peer = page.getByRole('dialog', { name: 'Add peer' })
  await expect(peer).toHaveCSS('opacity', '1')
  await page.screenshot({ path: testInfo.outputPath('network-peer-add.png') })
})

test('captures the review of a VLAN that takes its parent over', async ({ page }, testInfo) => {
  await mockNetworkBench(page)
  test.skip(testInfo.project.name !== 'chromium', 'one browser is enough for review captures')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto('./network')
  await page.getByRole('button', { name: 'Add' }).click()
  const wizard = page.getByRole('dialog')
  await wizard.getByRole('button', { name: 'VLAN' }).click()
  await wizard.getByRole('combobox').click()
  await page.getByRole('option', { name: 'eth2' }).click()
  await wizard.getByRole('textbox', { name: 'VLAN ID' }).fill('7')
  await wizard.getByRole('button', { name: 'Stage' }).click()
  await page.getByRole('button', { name: 'Review and apply' }).click()
  const adopting = page.getByRole('dialog', { name: 'Review network changes' })
  await expect(adopting.getByText('eth2 is declared with the configuration it runs now, so it can carry the VLAN.')).toBeVisible()
  await expect(adopting).toHaveCSS('opacity', '1')
  await page.screenshot({ path: testInfo.outputPath('network-review-adopt.png') })
})

test('captures the N7 network fixes in Chinese', async ({ page }, testInfo) => {
  await mockNetworkBench(page)
  test.skip(testInfo.project.name !== 'chromium', 'one browser is enough for review captures')
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.addInitScript(() => localStorage.setItem('mica.ui.locale', 'zh-CN'))

  // G1: a port switched to static gets no address of its own choosing, and
  // the lease it holds is one click away.
  await page.goto('./network/eth0')
  await page.getByRole('button', { name: '编辑配置' }).click()
  await page.getByRole('button', { name: '静态' }).click()
  await expect(page.getByRole('textbox', { name: 'IP 地址', exact: true })).toHaveValue('')
  await page.screenshot({ path: testInfo.outputPath('n7-static-empty.png'), fullPage: true })
  await page.getByRole('button', { name: '填入当前租约（192.168.1.24/24，网关 192.168.1.1）' }).click()
  await expect(page.getByRole('textbox', { name: 'IP 地址', exact: true })).toHaveValue('192.168.1.24/24')
  await expect(page.getByRole('textbox', { name: 'IP 地址', exact: true })).toHaveAttribute('aria-invalid', 'false')
  await page.getByText('配置', { exact: true }).click()
  await page.screenshot({ path: testInfo.outputPath('n7-static-lease.png'), fullPage: true })

  // G2 starts from no staged changes.
  await page.evaluate(() => sessionStorage.clear())

  // G2: the route table, the uplink and the standby.
  await page.goto('./network?tab=overview')
  await expect(page.getByText('路由表 · 9 条 · 默认经 eth0')).toBeVisible()
  await expect(page.getByText('备用 · wlan0')).toBeVisible()
  await page.setViewportSize({ width: 1440, height: 1400 })
  await page.screenshot({ path: testInfo.outputPath('n7-overview-routes.png') })
  await page.setViewportSize({ width: 1440, height: 1000 })

  // A new tunnel starts from free values; deleting one is staged behind a confirmation.
  await page.goto('./network?tab=wireguard')
  await page.getByRole('button', { name: '新增' }).click()
  await expect(page.getByRole('textbox', { name: '监听端口' })).toHaveValue('51821')
  await page.screenshot({ path: testInfo.outputPath('n7-wg-wizard.png'), fullPage: true })
  await page.getByRole('button', { name: '取消' }).click()
  await page.goto('./network?tab=wireguard&tunnel=wg0')
  await page.getByRole('button', { name: '删除 wg0' }).click()
  await expect(page.getByRole('alertdialog')).toContainText('1 个对端将断开，允许的网段 10.10.0.1/32 的路由将移除')
  await page.screenshot({ path: testInfo.outputPath('n7-wg-delete-confirm.png'), fullPage: true })
  await page.getByRole('alertdialog').getByRole('button', { name: '删除 wg0' }).click()
  await expect(page.getByText('删除 · 待应用')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('n7-wg-delete-pending.png'), fullPage: true })

  // A route list changes entry by entry in the review.
  await page.goto('./network/eth1')
  await page.getByRole('button', { name: '编辑配置' }).click()
  await page.getByRole('button', { name: '删除' }).first().click()
  await page.getByRole('button', { name: '添加路由' }).click()
  await page.getByRole('textbox', { name: '目标网段' }).last().fill('10.40.0.0/16')
  await page.getByRole('textbox', { name: '下一跳' }).last().fill('192.168.10.4')
  await page.getByRole('button', { name: '完成编辑' }).click()
  await page.getByRole('button', { name: '审阅并应用' }).click()
  await expect(page.getByRole('dialog', { name: '审阅网络更改' })).toContainText('+ 10.40.0.0/16 → 192.168.10.4')
  await page.screenshot({ path: testInfo.outputPath('n7-review-routes.png'), fullPage: true })
  await page.keyboard.press('Escape')

  // CAN reads in words.
  await page.goto('./network/can0')
  await expect(page.getByText('总线告警').first()).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('n7-can.png'), fullPage: true })

  // G3: each block's title, explanation and actions head its card.
  await page.evaluate(() => sessionStorage.clear())
  for (const [name, path, heading] of [
    ['interface', './network/eth1', '配置'],
    ['wifi', './network?tab=wifi', 'Wi-Fi 配置'],
    ['dns', './network?tab=dns', '静态解析记录'],
  ] as const) {
    await page.goto(path)
    await expect(page.getByRole('heading', { name: heading, exact: true })).toBeVisible()
    await page.waitForLoadState('networkidle')
    await page.screenshot({ path: testInfo.outputPath(`n7-card-${name}.png`), fullPage: true })
  }
  await page.goto('./network/eth1')
  await page.getByRole('button', { name: '编辑配置' }).click()
  await page.screenshot({ path: testInfo.outputPath('n7-card-editing.png'), fullPage: true })
  await page.getByRole('button', { name: '取消' }).click()
  await page.setViewportSize({ width: 560, height: 1000 })
  await page.screenshot({ path: testInfo.outputPath('n7-card-narrow.png'), fullPage: true })
  await page.setViewportSize({ width: 1440, height: 1000 })

  // A tunnel reads its state from its peers' handshakes.
  await page.goto('./network?tab=wireguard')
  await expect(page.getByText('隧道已连通')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('n7-wg-list.png'), fullPage: true })
  await page.goto('./network?tab=wireguard&tunnel=wg0')
  await expect(page.getByRole('columnheader', { name: '最近握手' })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('n7-wg-detail.png'), fullPage: true })
})

test('captures the device code, the claim key, BLE provisioning and the pairing window', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'one browser is enough for review captures')
  await page.setViewportSize({ width: 1440, height: 1000 })
  // The information tab reads more than the shared mocks answer.
  await page.route('**/api/v1/system/info', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({
    ...(payload('/api/v1/system/info', 'GET') as object),
    deployment: { available: true, id: 'mica-2026.08.2', version: '2026.08.2' },
  }) }))
  await page.route('**/api/v1/system/telemetry', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({
    thermal: { available: true, zones: [{ sensor: 'thermal_zone0', label: 'cpu', milliCelsius: 48200 }] },
    watchdog: { available: true, devices: [] },
    reset: { available: true, reason: 'unknown' },
  }) }))
  await page.goto('./system#information')
  await expect(page.getByText('DFN4-L34C-T8N · emmc-cid')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('system-device-code.png') })

  // Bluetooth: closed, then open with a phone waiting on the six digits.
  let open = false
  const declared = { 'AA:BB:CC:DD:EE:01': { name: 'Bridge tablet', trusted: true, blocked: false } }
  // BLE provisioning: off with its history, then a manual window with its code.
  let manual = false
  let verifierReady = false
  const history = [
    { at: 1_792_051_200, trigger: 'boot', outcome: 'wifi-connected' },
    { at: 1_791_964_800, trigger: 'offline', outcome: 'auth-failed' },
    { at: 1_791_878_400, trigger: 'unclaimed', outcome: 'claimed' },
  ]
  await page.route('**/api/v1/bluetooth**', async (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (path === '/api/v1/bluetooth/provisioning') {
      if (request.method() === 'POST') manual = true
      if (request.method() === 'DELETE') {
        manual = false
        return route.fulfill({ status: 204 })
      }
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({
        state: manual ? 'waiting' : 'off',
        trigger: manual ? 'manual' : null,
        expiresAt: manual ? Math.floor(Date.now() / 1000) + 600 : null,
        ...(manual ? { code: '20481337' } : {}),
        autoWhenOffline: true,
        verifierReady,
        lockedUntil: null,
        wifiLocked: false,
        history,
      }) })
    }
    if (path === '/api/v1/bluetooth/pairing-window') {
      open = request.method() === 'POST'
      return route.fulfill(open
        ? { status: 200, contentType: 'application/json', body: JSON.stringify({ expiresAt: Math.floor(Date.now() / 1000) + 180 }) }
        : { status: 204 })
    }
    return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({
      enabled: true,
      declared,
      adapter: { available: true, address: 'EC:B5:0A:97:9D:25', name: 'mica-cm4', powered: true, discoverable: open },
      devices: { available: true, entries: [{ address: 'AA:BB:CC:DD:EE:01', name: 'Bridge tablet', paired: true, trusted: true, blocked: false, connected: true, rssi: -52 }] },
      pending: open ? [{ name: 'Pixel 8', mac: '6C:2F:8A:41:D3:5B', passkey: '042137', since: 0 }] : [],
      pairWindow: open ? { expiresAt: Math.floor(Date.now() / 1000) + 163 } : null,
    }) })
  })
  // Tall enough for every panel: a full-page capture puts the fixed status bar over the requests.
  await page.setViewportSize({ width: 1440, height: 2000 })
  await page.goto('./network?tab=bluetooth')
  await expect(page.getByText(/cannot normally be discovered/)).toBeVisible()
  await expect(page.getByText(/Sign in to the console once/)).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('bluetooth-verifier-missing.png') })
  verifierReady = true
  await page.reload()
  await expect(page.getByText('init.micaos.dev')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('bluetooth.png') })
  await page.getByRole('button', { name: 'Edit configuration' }).click()
  await page.getByRole('switch', { name: 'Provisioning when offline' }).click()
  await page.screenshot({ path: testInfo.outputPath('bluetooth-config-edit.png') })
  await page.getByRole('button', { name: 'Cancel' }).click()
  await page.getByRole('button', { name: 'Open provisioning' }).click()
  await expect(page.getByTestId('provisioning-code')).toHaveText('2048 1337')
  await page.screenshot({ path: testInfo.outputPath('bluetooth-provisioning.png') })
  await page.getByRole('button', { name: 'Close provisioning' }).click()
  await expect(page.getByRole('button', { name: 'Open provisioning' })).toBeVisible()
  await page.getByRole('button', { name: 'Allow new devices' }).click()
  await expect(page.getByText('042137')).toBeVisible()
  await expect(page.getByTestId('pair-window')).toContainText('Allowing pairing')
  await page.screenshot({ path: testInfo.outputPath('bluetooth-window.png') })

  // A provisioning session holds the radio: the Wi-Fi page is locked.
  await page.unroute('**/api/v1/bluetooth**')
  await page.route('**/api/v1/bluetooth/provisioning', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({
    state: 'connected', trigger: 'boot', expiresAt: Math.floor(Date.now() / 1000) + 420, autoWhenOffline: true, verifierReady: true, lockedUntil: null, wifiLocked: true, history,
  }) }))
  await page.setViewportSize({ width: 1440, height: 1000 })
  await page.goto('./network?tab=wifi')
  await expect(page.getByText('Bluetooth provisioning in progress; the Wi-Fi configuration is locked.')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('wifi-locked.png') })

  // A device claimed over Bluetooth names the channel.
  await page.route('**/api/v1/claim', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ state: 'claimed', via: 'ble', at: 1_791_878_400, rotationRequired: false }) }))
  await page.goto('./access')
  await expect(page.getByText('Bluetooth provisioning')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('access-claim-ble.png') })

  // The claim: an optional key, and a generated one shown once.
  await page.route('**/api/v1/session', (route) => route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ state: 'setup' }) }))
  await page.route('**/api/v1/setup', (route) => route.fulfill({ status: 201, contentType: 'application/json', body: JSON.stringify({ csrfToken: 'e2e-csrf', apPsk: 'K7QM2XWD9HC4TR8N' }) }))
  await page.goto('./')
  await page.getByLabel('Admin password').fill('harbour-pw-1')
  await page.getByLabel('Confirm password').fill('harbour-pw-1')
  await page.screenshot({ path: testInfo.outputPath('setup.png') })
  await page.getByRole('button', { name: 'Configure device' }).click()
  await expect(page.getByTestId('generated-ap-psk')).toHaveText('K7QM2XWD9HC4TR8N')
  await page.screenshot({ path: testInfo.outputPath('setup-ap-key.png') })
})

test('captures an access point on the fallback channel', async ({ page }, testInfo) => {
  await mockNetworkBench(page)
  test.skip(testInfo.project.name !== 'chromium', 'one browser is enough for review captures')
  await page.setViewportSize({ width: 1440, height: 1000 })
  const json = (body: unknown) => ({ status: 200, contentType: 'application/json', body: JSON.stringify(body) })
  await page.route('**/api/v1/wifi/ap', (route) => route.fulfill(json({ ...(benchPayload('/api/v1/wifi/ap') as object), mode: 'always', ssid: 'mica-789Z', channel: 0 })))
  await page.route('**/api/v1/wifi/client', (route) => route.fulfill(json({ enabled: false, interface: 'wlan0' })))
  await page.route('**/api/v1/network/status', (route) => {
    const status = benchPayload('/api/v1/network/status') as Record<string, unknown>
    return route.fulfill(json({
      ...status,
      wifi: { available: true, associations: [] },
      accessPoint: {
        available: true,
        running: true,
        reason: 'acs_failed',
        reasonDetail: 'hostapd gave up choosing a channel (state DISABLED); running on channel 1',
        entries: [{ interface: 'wlan0', stationCount: 0, stations: [] }],
      },
    }))
  })
  await page.goto('./network')
  await expect(page.getByText(/automatic channel selection failed/).first()).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('network-acs-fallback.png'), fullPage: true })
})

test('captures the factory reset, its password and the reboot, in English and Chinese', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'one browser is enough for review captures')
  await page.setViewportSize({ width: 1440, height: 1000 })
  const posted: unknown[] = []
  await page.route('**/api/v1/system/factory-reset', (route) => {
    if (route.request().method() === 'GET') {
      return route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({
        supported: true,
        refusal: null,
        last: { requested: 1_760_000_000, completed: 1_760_000_045, by: 'console', source: '192.168.1.40', unremoved: 0 },
      }) })
    }
    const body = route.request().postDataJSON() as { password: string }
    posted.push(body)
    return body.password === 'hunter2secret'
      ? route.fulfill({ status: 202, contentType: 'application/json', body: JSON.stringify({ applies: 'reboot' }) })
      : route.fulfill({ status: 403, contentType: 'application/json', body: JSON.stringify({ error: { code: 'password_incorrect', message: 'the password is not the administrator\'s', source: 'micad', path: 'password' } }) })
  })
  for (const [locale, suffix, start, field, submit, rebooting] of [
    ['en', 'en', 'Factory reset…', 'Administrator password', 'Erase and reboot', 'The device is rebooting and erasing.'],
    ['zh-CN', 'zh', '恢复出厂设置…', '管理员密码', '清除并重启', '设备正在重启并清除数据。'],
  ] as const) {
    await page.addInitScript((value) => localStorage.setItem('mica.ui.locale', value), locale)
    await page.goto('./system#general')
    // The same address again does not reload, and the locale is read at load.
    await page.reload()
    const section = page.locator('section').filter({ has: page.getByRole('button', { name: start }) })
    await section.scrollIntoViewIfNeeded()
    await section.screenshot({ path: testInfo.outputPath(`factory-reset-${suffix}.png`), animations: 'disabled' })
    await page.getByRole('button', { name: start }).click()
    await page.getByLabel(field).fill('a guess')
    await page.getByRole('button', { name: submit }).click()
    await expect(page.getByText('the password is not the administrator\'s').first()).toBeVisible()
    await page.screenshot({ path: testInfo.outputPath(`factory-reset-wrong-${suffix}.png`), animations: 'disabled' })
    await page.getByLabel(field).fill('hunter2secret')
    await page.screenshot({ path: testInfo.outputPath(`factory-reset-dialog-${suffix}.png`), animations: 'disabled' })
    await page.getByRole('button', { name: submit }).click()
    await expect(page.getByText(rebooting)).toBeVisible()
    await section.screenshot({ path: testInfo.outputPath(`factory-reset-rebooting-${suffix}.png`), animations: 'disabled' })
  }
  expect(posted).toEqual([{ password: 'a guess' }, { password: 'hunter2secret' }, { password: 'a guess' }, { password: 'hunter2secret' }])
})

test('uses the mobile navigation drawer', async ({ page, isMobile }) => {
  test.skip(!isMobile, 'mobile navigation contract')
  await page.goto('./')

  await page.getByRole('button', { name: 'Open navigation' }).click()
  await expect(page.getByRole('navigation', { name: 'Primary navigation' })).toBeVisible()
  await page.getByRole('link', { name: 'Network' }).click()
  await expect(page.getByRole('heading', { name: 'Network' })).toBeVisible()
})

test('captures taking the API off the network', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'one browser is enough for review captures')
  await page.setViewportSize({ width: 1440, height: 1000 })
  const json = (body: unknown) => ({ status: 200, contentType: 'application/json', body: JSON.stringify(body) })
  await page.route('**/api/v1/web', (route) => route.request().method() === 'GET'
    ? route.fulfill(json({ mode: 'network', httpPort: 8080, httpsEnabled: false, httpsPort: 8443 }))
    : route.fulfill({ status: 202, contentType: 'application/json', body: JSON.stringify({ taskId: 'task-e2e' }) }))
  await page.goto('./access')
  const panel = page.getByRole('group', { name: 'API access' }).locator('xpath=ancestor::form[1]')
  await page.getByRole('button', { name: 'Device only' }).click()
  await expect(page.getByText('Only root on this device reaches the API')).toBeVisible()
  await panel.screenshot({ path: testInfo.outputPath('access-web-mode.png') })
  await page.getByRole('button', { name: 'Save and restart the console' }).click()
  await expect(page.getByRole('alertdialog')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('access-web-confirm.png'), animations: 'disabled' })
  await page.getByRole('button', { name: 'Leave the network' }).click()
  await expect(page.getByRole('heading', { name: 'API access is now device only' })).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('api-gone.png') })
})

test('captures the apps on the services page', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'one browser is enough for review captures')
  await page.setViewportSize({ width: 1440, height: 1000 })
  const json = (body: unknown, status = 200) => ({ status, contentType: 'application/json', body: JSON.stringify(body) })
  await page.route('**/api/v1/apps', (route) => route.request().method() === 'GET'
    ? route.fulfill(json({ available: true, arch: 'arm64', apps: [TAILSCALE, HARBOUR_AGENT] }))
    : route.fulfill(json({ error: {
      code: 'app_invalid',
      message: 'the bundle breaks 2 rules',
      rules: [
        { rule: 'arch', detail: 'the bundle is for amd64; this device is arm64' },
        { rule: 'unit_shipped_by_root', detail: 'sshd.service is shipped by the root' },
      ],
    } }, 422)))
  await page.route('**/api/v1/apps/tailscale/logs', (route) => route.fulfill(json({
    name: 'tailscale',
    units: [{ unit: 'tailscaled.service', available: true, lines: [
      '2026-10-21T09:12:03Z tailscaled: logtail started',
      '2026-10-21T09:12:04Z tailscaled: magicsock: home is now derp-14 (ams)',
      '2026-10-21T09:12:05Z tailscaled: health(warnable=login-state): ok',
    ] }],
    firstRun: ['tailscale: state directory /srv/apps/tailscale ready'],
  })))
  await page.goto('./services')
  await expect(page.getByRole('link', { name: 'tailscale' })).toBeVisible()
  await page.locator('input[type=file]').setInputFiles({ name: 'tailscale-amd64.tar.gz', mimeType: 'application/gzip', buffer: Buffer.from('bundle') })
  await page.getByRole('button', { name: 'Install', exact: true }).click()
  await expect(page.getByText('sshd.service is shipped by the root')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('services-apps.png'), animations: 'disabled', fullPage: true })
  // A fresh load, so the refusal's toast is not over the detail page.
  await page.goto('./services/apps/tailscale')
  await expect(page.getByRole('heading', { name: 'tailscale' })).toBeVisible()
  await expect(page.getByText('magicsock')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('app-detail.png'), animations: 'disabled', fullPage: true })
  await page.route('**/api/v1/apps/harbour-agent/logs', (route) => route.fulfill(json({ name: 'harbour-agent', units: [], firstRun: null })))
  await page.goto('./services/apps/harbour-agent')
  await expect(page.getByText('This app runs as root')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('app-detail-root.png'), animations: 'disabled' })
  // A bundle asking for more than the installed version is asked about.
  await page.route('**/api/v1/apps', (route) => route.request().method() === 'GET'
    ? route.fulfill(json({ available: true, arch: 'arm64', apps: [TAILSCALE, HARBOUR_AGENT] }))
    : route.fulfill(json({ error: {
      code: 'app_privileges',
      message: 'tailscale 1.104.2 asks for privileges beyond 1.102.1; accept them to install it',
      source: 'micad',
      params: { root: false, capabilities: ['CAP_NET_BIND_SERVICE'], devices: ['/dev/ttyUSB0'] },
    } }, 409)))
  await page.goto('./services')
  await page.locator('input[type=file]').setInputFiles({ name: 'tailscale-1.104.2-arm64.tar.gz', mimeType: 'application/gzip', buffer: Buffer.from('bundle') })
  await page.getByRole('button', { name: 'Install', exact: true }).click()
  await expect(page.getByRole('alertdialog').getByText('Devices: /dev/ttyUSB0')).toBeVisible()
  await page.screenshot({ path: testInfo.outputPath('app-privileges.png'), animations: 'disabled' })
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
  await page.goto('./system?tab=information')
  await page.getByRole('button', { name: 'Reboot' }).click()
  const dialog = page.getByRole('alertdialog')
  await expect(dialog).toBeVisible()
  await dialog.getByRole('button', { name: 'Reboot' }).click()

  await expect(dialog).toHaveCount(0)
  await expect(page.getByText('Power action accepted.').first()).toBeVisible()
})

test('reports a saved policy', async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== 'chromium', 'one browser covers the feedback contract')
  await page.goto('./system?tab=information')
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
  if (path === '/api/v1/device/code') return { code: 'DFN4-L34C-T8N', source: 'emmc-cid' }
  if (path === '/api/v1/system/factory-reset') return { supported: true, refusal: null, last: null }
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
  if (path === '/api/v1/apps') return { available: true, arch: 'arm64', apps: [] }
  return {}
}

/// A bench device for the network pages: an uplink, a statically addressed LAN,
/// an unplugged port, a joined radio and a tunnel with one peer. Registered
/// after the device mock, so its answers take precedence.
async function mockNetworkBench(page: Page) {
  await page.route('**/api/v1/**', async (route) => {
    const request = route.request()
    const url = new URL(request.url())
    const body = request.method() === 'GET'
      ? benchPayload(url.pathname)
      : request.method() === 'PUT' && url.pathname === '/api/v1/network' && url.searchParams.get('dryRun') === 'true'
        ? dryRun(request.postDataJSON() as { configured: Record<string, { vlan?: { parent: string } }> })
        : undefined
    if (body === undefined) return route.fallback()
    await route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify(body) })
  })
}

const TAILSCALE = {
  name: 'tailscale',
  version: '1.102.1',
  description: 'Tailscale node for remote access to this device',
  arch: ['amd64', 'arm64'],
  sha256: '9f2c4e1a7b3d5f60c8e2a4b6d8f0123456789abcdef0123456789abcdef01234',
  source: 'upload',
  installedAt: '2026-10-20T16:40:00Z',
  previous: '1.100.0',
  units: [{ name: 'tailscaled.service', enabled: true, active: 'active' }],
  privileges: { user: 'app', account: 'mica-app-tailscale', uid: 30000, capabilities: ['CAP_NET_ADMIN', 'CAP_NET_RAW'], devices: ['/dev/net/tun'] },
  last: { action: 'upgrade', outcome: 'ok', at: '2026-10-20T16:40:12Z' },
  update: { url: 'https://pkgs.example.eu/mica/tailscale/latest.json', auto: false, available: '1.104.2', checkedAt: '2026-10-21T06:00:00Z', error: null },
  seed: null,
}

const HARBOUR_AGENT = {
  ...TAILSCALE,
  name: 'harbour-agent',
  version: '3.2.0',
  description: 'Marina berth sensors, by the harbour office',
  source: 'seed',
  previous: null,
  units: [{ name: 'harbour-agent.service', enabled: true, active: 'active' }],
  privileges: { user: 'root', account: null, uid: null, capabilities: [], devices: [] },
  update: null,
}

const PEER = { name: 'Harbour office', publicKey: 'cmVtb3RlUGVlcktleXJlbW90ZVBlZXJLZXlyZW1vdGU=', presharedKey: '<redacted>', allowedIps: ['10.10.0.1/32'], endpoint: 'vpn.example.eu:51820', persistentKeepalive: 25 }

/// A dry run on the bench: each VLAN parent the map does not declare is
/// taken over, as the device does with an image that gives it DHCP.
function dryRun(write: { configured: Record<string, { vlan?: { parent: string } }> }) {
  const parents = Object.values(write.configured).flatMap((entry) => entry.vlan ? [entry.vlan.parent] : [])
  return { adopted: [...new Set(parents.filter((parent) => write.configured[parent] === undefined))], warnings: [], diff: [] }
}

function benchPayload(path: string): unknown {
  if (path === '/api/v1/network') return {
    configured: {
      eth0: { dhcp: true },
      eth1: { dhcp: false, static: { address: '192.168.10.1/24' }, routes: [{ destination: '10.1.1.0/24', gateway: '192.168.10.2' }, { destination: '10.30.0.0/16', gateway: '192.168.10.3' }] },
      wg0: { kind: 'wireguard', dhcp: false, static: { address: '10.10.0.2/24', dns: [] }, wireguard: { listenPort: 51820, peers: [PEER] } },
      can0: { kind: 'can', can: { bitrate: 250000 } },
    },
    configuredCount: 4,
    version: 'e2e-version',
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
  if (path === '/api/v1/network/wireguard/defaults') return { name: 'wg1', address: '10.2.0.1/24', listenPort: 51821, udpPortsInUse: [5353] }
  if (path === '/api/v1/network/wg0/peers') return [{ ...PEER, presharedKey: true, latestHandshake: Math.floor(Date.now() / 1000) - 42, rxBytes: 1_284_000, txBytes: 312_000 }]
  if (path === '/api/v1/state/network') return { wg0: { publicKey: 'e2eDevicePublicKeye2eDevicePublicKeye2eDev0=' } }
  if (path === '/api/v1/wifi/client') return { enabled: true, interface: 'wlan0' }
  if (path === '/api/v1/wifi/ap') return { mode: 'off', interface: 'wlan0', ssid: 'mica-cm4', psk: '<redacted>', channel: 6, countryCode: 'DE', address: '192.168.4.1/24', holdDownSeconds: 120, graceSeconds: 60, dhcpServer: { poolStart: '192.168.4.2', poolEnd: '192.168.4.254' } }
  if (path === '/api/v1/wifi/channels') return { countryCode: 'DE', channels: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13], auto: true }
  if (path === '/api/v1/wifi/client/networks') return [
    { ssid: 'marina-guest', psk: '<redacted>', hidden: false, priority: 10 },
    { ssid: 'fuel-dock', psk: '<redacted>', hidden: false, priority: 1, autoConnect: false },
  ]
  if (path === '/api/v1/network/status') return {
    interfaces: { available: true, count: 6, entries: [
      {
        name: 'eth0', index: 2, hardwareAddress: '02:42:ac:11:00:02',
        link: { operationalState: 'routable', carrierState: 'carrier', carrier: true },
        addresses: [{ family: 'inet', address: '192.168.1.24', prefixLength: 24, scope: 'global', configSource: 'DHCPv4' }],
        dhcp: { available: true, state: 'bound' },
        lease: { available: true, server: '192.168.1.1', router: '192.168.1.1', remainingSeconds: 3480 },
        dns: ['192.168.1.1'],
        linkDns: [{ address: '192.168.1.1', source: 'DHCPv4' }],
        routes: [
          { dest: '0.0.0.0/0', family: 'ipv4', via: '192.168.1.1', metric: 100, source: 'DHCPv4', table: 'main', protocol: 'dhcp', scope: 'global' },
          { dest: '192.168.1.0/24', family: 'ipv4', via: null, metric: 100, source: 'DHCPv4', table: 'main', protocol: 'kernel', scope: 'link' },
          { dest: '192.168.1.1/32', family: 'ipv4', via: null, metric: 100, source: 'DHCPv4', table: 'main', protocol: 'dhcp', scope: 'link' },
          { dest: 'fe80::/64', family: 'ipv6', via: null, metric: 256, source: null, table: 'main', protocol: 'kernel', scope: 'link' },
        ],
      },
      {
        name: 'eth1', index: 3, hardwareAddress: '02:42:ac:11:00:03',
        link: { operationalState: 'routable', carrierState: 'carrier', carrier: true },
        addresses: [{ family: 'inet', address: '192.168.10.1', prefixLength: 24, scope: 'global', configSource: 'static' }],
        dhcp: { available: false }, dns: [],
        routes: [
          { dest: '10.1.1.0/24', family: 'ipv4', via: '192.168.10.2', metric: 0, source: 'static', table: 'main', protocol: 'static', scope: 'global' },
          { dest: '10.30.0.0/16', family: 'ipv4', via: '192.168.10.3', metric: 0, source: 'static', table: 'main', protocol: 'static', scope: 'global' },
          { dest: '192.168.10.0/24', family: 'ipv4', via: null, metric: 0, source: 'static', table: 'main', protocol: 'kernel', scope: 'link' },
        ],
      },
      { name: 'eth2', index: 4, link: { operationalState: 'no-carrier', carrier: false }, addresses: [], dhcp: { available: false }, dns: [] },
      {
        name: 'wlan0', index: 5, type: 'wlan',
        link: { operationalState: 'routable', carrier: true },
        addresses: [{ family: 'inet', address: '10.20.0.31', prefixLength: 24, scope: 'global', configSource: 'DHCPv4' }],
        dhcp: { available: true }, dns: [],
        routes: [
          { dest: '0.0.0.0/0', family: 'ipv4', via: '10.20.0.1', metric: 600, source: 'DHCPv4', table: 'main', protocol: 'dhcp', scope: 'global' },
          { dest: '10.20.0.0/24', family: 'ipv4', via: null, metric: 600, source: 'DHCPv4', table: 'main', protocol: 'kernel', scope: 'link' },
        ],
      },
      {
        name: 'wg0', index: 6, kind: 'wireguard', link: { operationalState: 'routable', carrier: true }, addresses: [{ family: 'inet', address: '10.10.0.2', prefixLength: 24, scope: 'global' }], dhcp: { available: false }, dns: [],
        routes: [{ dest: '10.10.0.0/24', family: 'ipv4', via: null, metric: 0, source: 'static', table: 'main', protocol: 'kernel', scope: 'link' }],
        wireguard: { available: true, peers: [{ publicKey: PEER.publicKey, latestHandshake: Math.floor(Date.now() / 1000) - 42, rxBytes: 1_284_000, txBytes: 312_000, persistentKeepalive: 25 }] },
      },
      {
        name: 'can0', index: 7, type: 'can', link: { operationalState: 'carrier', carrier: true }, addresses: [], dhcp: { available: false }, dns: [],
        can: { available: true, busState: 'error-warning', bitrate: 250000, samplePoint: 0.875, txErrors: 96, rxErrors: 3, warningCount: 4, errorPassiveCount: 0, busOffCount: 0 },
      },
    ] },
    session: { interface: 'eth0', localAddress: '192.168.1.24' },
    defaultRoutes: { available: true, count: 2, entries: [
      { family: 'ipv4', gateway: '192.168.1.1', interface: 'eth0', metric: 100, effective: true },
      { family: 'ipv4', gateway: '10.20.0.1', interface: 'wlan0', metric: 600, effective: false },
    ] },
    dns: { available: true, linkServers: ['192.168.1.1'], resolverServers: ['127.0.0.53'], probe: { name: 'deb.debian.org', reachable: true, result: 'resolved' } },
    wifi: { available: true, associations: [{ interface: 'wlan0', associated: true, ssid: 'marina-guest', rssiDbm: -58 }] },
    accessPoint: { available: true, entries: [] },
    capabilities: { wifi: { supported: true, interfaces: ['wlan0'] }, bluetooth: { supported: false, adapters: [] }, cellular: { supported: false, interfaces: [] } },
  }
  return undefined
}
