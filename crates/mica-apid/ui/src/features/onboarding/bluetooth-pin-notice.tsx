import { useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { useTranslation } from 'react-i18next'
import { api } from '@/shared/lib/http'
import type { BluetoothOverview } from '@/lib/types'
import type { Feature } from '@/shared/lib/features'
import { ConfirmDialog } from '@/shared/components/confirm-dialog'
import { DEFAULT_PIN } from '@/features/network/bluetooth'

/// Where this browser remembers the code its operator said they saved.
export const PIN_SAVED_KEY = 'mica.bluetoothPinSaved'

/// The pairing code, put in front of the operator on the first sign-in, on a
/// product with Bluetooth and with Bluetooth on.
///
/// The claim replaces the default code with six random digits, and a phone
/// that pairs later needs them. The dialog comes back on every load until the
/// operator says the code is saved, and again when the code changes. The
/// default code is not one to save: the Bluetooth panel asks for a new one.
export function BluetoothPinNotice() {
  const { t } = useTranslation()
  // Only once the device has said it serves Bluetooth: a product without it
  // has no code to save, and its route answers 404.
  const meta = useQuery({
    queryKey: ['meta'],
    queryFn: () => api<{ features?: Feature[] }>('/api/v1/meta'),
    staleTime: Infinity,
    retry: false,
  })
  const served = meta.data?.features?.includes('bluetooth') === true
  const bluetooth = useQuery({
    queryKey: ['bluetooth'],
    queryFn: () => api<BluetoothOverview>('/api/v1/bluetooth'),
    enabled: served,
  })
  const [saved, setSaved] = useState(() => window.localStorage.getItem(PIN_SAVED_KEY))
  const [later, setLater] = useState(false)
  const pin = bluetooth.data?.pin
  // Switched off, the adapter pairs nothing and the code is not the one it
  // will offer: it is replaced when Bluetooth is switched on.
  if (!served || !bluetooth.data?.enabled || !pin || pin === DEFAULT_PIN || saved === pin || later) return null
  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => { if (!open) setLater(true) }}
      tone="neutral"
      title={t('shell.bluetoothPin.title')}
      description={<>
        {t('shell.bluetoothPin.copy')}
        <span className="mt-3 block font-mono text-3xl tracking-widest text-foreground">{pin}</span>
      </>}
      confirmLabel={t('shell.bluetoothPin.saved')}
      cancelLabel={t('shell.bluetoothPin.later')}
      success={t('shell.bluetoothPin.recorded')}
      failure={t('shell.bluetoothPin.title')}
      onConfirm={() => {
        window.localStorage.setItem(PIN_SAVED_KEY, pin)
        setSaved(pin)
      }}
    />
  )
}
