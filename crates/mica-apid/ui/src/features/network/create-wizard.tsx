import { useState } from 'react'
import { useNavigate } from '@tanstack/react-router'
import { useTranslation } from 'react-i18next'
import { FormField } from '@/shared/components/form-field'
import { SegmentedControl } from '@/shared/components/segmented-control'
import { Button } from '@/shared/components/ui/button'
import { Dialog, DialogBody, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/shared/components/ui/dialog'
import { Input } from '@/shared/components/ui/input'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select'
import { updateChangeSet } from './change-set'
import { bridgeOf, NAME_PATTERN, nextName, overlaps, parseCidr, stageCreate, vlansOn, type CreateSpec } from './model'
import type { NetworkData } from './use-network'

type Kind = CreateSpec['kind']

/// A new bridge, VLAN or tunnel, staged with the other changes. Ethernet ports
/// are the hardware's and appear on their own.
export function CreateWizard({ data, kinds, onClose }: { data: NetworkData; kinds: Kind[]; onClose: () => void }) {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const view = data.staged
  const map = view.map
  const [kind, setKind] = useState<Kind>(kinds[0])
  const [names, setNames] = useState<Partial<Record<Kind, string>>>({})
  const parents = view.order.filter((name) => view.ifaces[name].kind === 'physical' || view.ifaces[name].kind === 'bridge')
  const [parent, setParent] = useState(() => parents.find((name) => !bridgeOf(map, name) && name !== data.session) ?? parents[0] ?? '')
  const [vlanId, setVlanId] = useState('')
  const [address, setAddress] = useState('10.2.0.1/24')
  const [port, setPort] = useState('51821')
  const taken = (name: string) => view.ifaces[name] !== undefined

  const automatic = kind === 'bridge' ? nextName(taken, 'br', 1) : kind === 'vlan' ? (vlanId ? `${parent}-${vlanId}` : '') : nextName(taken, 'wg', 0)
  const name = names[kind] ?? automatic
  const errors: Partial<Record<'name' | 'vlanId' | 'parent' | 'address' | 'port', string>> = {}
  if (!name) { if (kind !== 'vlan' || vlanId) errors.name = t('network.problems.nameMissing') }
  else if (!NAME_PATTERN.test(name)) errors.name = t(name.length > 15 ? 'network.problems.nameLong' : 'network.problems.nameChars')
  else if (taken(name)) errors.name = t('network.problems.nameTaken')
  if (kind === 'vlan') {
    const id = Number(vlanId)
    if (!vlanId) errors.vlanId = t('network.problems.vlanRange')
    else if (!(id >= 1 && id <= 4094)) errors.vlanId = t('network.problems.vlanRange')
    else if (vlansOn(map, parent).some((vlan) => map[vlan].vlan?.id === id)) errors.vlanId = t('network.problems.vlanTaken', { parent, id })
    const bridge = bridgeOf(map, parent)
    if (bridge) errors.parent = t('network.problems.parentInBridge', { bridge })
    if (!parent) errors.parent = t('network.problems.parentMissing')
  }
  if (kind === 'wireguard') {
    const clash = view.order.find((each) => { const held = view.ifaces[each].obs?.address ?? (!map[each]?.dhcp ? map[each]?.static?.address : undefined); return held !== undefined && overlaps(address, held) })
    if (!parseCidr(address)) errors.address = t('network.problems.cidr')
    else if (clash) errors.address = t('network.problems.overlapWith', { name: clash })
    const listen = Number(port)
    if (!(listen >= 1 && listen <= 65535)) errors.port = t('network.problems.portRange')
    else if (Object.values(map).some((entry) => entry.kind === 'wireguard' && entry.wireguard?.listenPort === listen)) errors.port = t('network.problems.portTaken')
  }
  const valid = Object.keys(errors).length === 0 && name !== ''

  const create = () => {
    updateChangeSet((set) => stageCreate(set, kind === 'wireguard'
      ? { kind, name, address, listenPort: Number(port) }
      : kind === 'vlan' ? { kind, name, parent, vlanId: Number(vlanId) } : { kind, name }))
    onClose()
    if (kind !== 'wireguard') void navigate({ to: '/network/$name', params: { name } })
  }
  const parentUndeclared = kind === 'vlan' && parent !== '' && map[parent] === undefined

  return (
    <Dialog open onOpenChange={(open) => { if (!open) onClose() }}>
      <DialogContent showCloseButton={false}>
        <DialogHeader>
          <DialogTitle>{kinds.length > 1 ? t('network.wizard.title') : t('network.wizard.titleOf', { kind: t(`network.kinds.${kinds[0]}`) })}</DialogTitle>
          <DialogDescription>{t('network.wizard.description')}{kinds.length > 1 ? ` ${t('network.wizard.hardware')}` : ''}</DialogDescription>
        </DialogHeader>
        <DialogBody className="grid gap-4 py-1">
          {kinds.length > 1 ? (
            <FormField label={t('network.wizard.kind')}>
              {() => <SegmentedControl<Kind> label={t('network.wizard.kind')} value={kind} onValueChange={setKind} segments={kinds.map((each) => ({ value: each, label: t(`network.kinds.${each}`) }))} />}
            </FormField>
          ) : null}
          <FormField label={t('network.wizard.name')} hint={errors.name ? <span className="text-destructive">{errors.name}</span> : t(names[kind] === undefined ? 'network.wizard.nameAuto' : 'network.wizard.nameHint')}>
            {(id) => <Input id={id} className="font-mono" value={name} aria-invalid={Boolean(errors.name)} placeholder={kind === 'vlan' ? `${parent}-<VLAN ID>` : ''} onChange={(event) => setNames({ ...names, [kind]: event.target.value })} />}
          </FormField>
          {kind === 'vlan' ? (
            <>
              <FormField label={t('network.fields.parent')} hint={errors.parent ? <span className="text-destructive">{errors.parent}</span> : undefined}>
                {(id) => (
                  <Select value={parent} onValueChange={(value) => setParent(String(value))}>
                    <SelectTrigger id={id} className="font-mono"><SelectValue /></SelectTrigger>
                    <SelectContent>{parents.map((each) => <SelectItem key={each} value={each} disabled={bridgeOf(map, each) !== undefined}>{each}{bridgeOf(map, each) ? ` (${t('network.wizard.inBridge', { bridge: bridgeOf(map, each) })})` : ''}</SelectItem>)}</SelectContent>
                  </Select>
                )}
              </FormField>
              <FormField label={t('network.fields.vlanId')} hint={vlanId && errors.vlanId ? <span className="text-destructive">{errors.vlanId}</span> : t('network.wizard.vlanHint')}>
                {(id) => <Input id={id} className="w-32 font-mono" inputMode="numeric" value={vlanId} placeholder="1–4094" onChange={(event) => setVlanId(event.target.value.replace(/\D/g, ''))} />}
              </FormField>
              {parentUndeclared ? <p className="text-sm text-muted-foreground">{t('network.wizard.parentTaken', { parent })}</p> : null}
            </>
          ) : null}
          {kind === 'wireguard' ? (
            <>
              <FormField label={t('network.fields.tunnelAddress')} hint={errors.address ? <span className="text-destructive">{errors.address}</span> : t('network.wizard.tunnelHint')}>
                {(id) => <Input id={id} className="font-mono" value={address} aria-invalid={Boolean(errors.address)} onChange={(event) => setAddress(event.target.value.trim())} />}
              </FormField>
              <FormField label={t('network.fields.listenPort')} hint={errors.port ? <span className="text-destructive">{errors.port}</span> : t('network.wizard.portHint')}>
                {(id) => <Input id={id} className="w-32 font-mono" inputMode="numeric" value={port} onChange={(event) => setPort(event.target.value.replace(/\D/g, ''))} />}
              </FormField>
            </>
          ) : null}
          {kind === 'bridge' ? <p className="text-sm text-muted-foreground">{t('network.wizard.bridgeHint')}</p> : null}
        </DialogBody>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>{t('common.actions.cancel')}</Button>
          <Button disabled={!valid} onClick={create}>{t('network.wizard.stage')}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
