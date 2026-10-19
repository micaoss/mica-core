import { useState, type ReactElement, type ReactNode } from 'react'
import { useTranslation } from 'react-i18next'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
  AlertDialogTrigger,
} from '@/shared/components/ui/alert-dialog'
import { Spinner } from '@/shared/components/ui/spinner'
import { failureDetail, notifyFailure, notifySuccess } from '@/shared/feedback/toast'
import type { Tone } from './callout'

/// The console's only confirmation.
export function ConfirmDialog({
  trigger,
  title,
  description,
  confirmLabel,
  cancelLabel,
  tone = 'danger',
  success,
  failure,
  onConfirm,
  open,
  onOpenChange,
  disabled,
}: {
  /// Omitted when the dialog is driven by `open` from somewhere else, such as a
  /// form that confirms on submit.
  trigger?: ReactElement
  title: string
  description: ReactNode
  confirmLabel: string
  cancelLabel?: string
  tone?: Extract<Tone, 'danger' | 'neutral'>
  success: string
  failure: string
  onConfirm: () => Promise<unknown> | unknown
  open?: boolean
  onOpenChange?: (open: boolean) => void
  disabled?: boolean
}) {
  const { t } = useTranslation()
  const [uncontrolled, setUncontrolled] = useState(false)
  const [pending, setPending] = useState(false)
  const isOpen = open ?? uncontrolled
  const setOpen = (next: boolean) => {
    // A confirmation in flight is not dismissible: closing it would leave the
    // operator with no report of an operation that is already running.
    if (pending && !next) return
    onOpenChange?.(next)
    if (open === undefined) setUncontrolled(next)
  }

  const confirm = async () => {
    setPending(true)
    try {
      await onConfirm()
      notifySuccess(success)
      setPending(false)
      onOpenChange?.(false)
      if (open === undefined) setUncontrolled(false)
    } catch (error) {
      setPending(false)
      notifyFailure(failure, failureDetail(error, t('common.requestFailed')))
    }
  }

  return (
    <AlertDialog open={isOpen} onOpenChange={setOpen}>
      {trigger ? <AlertDialogTrigger disabled={disabled} render={trigger} /> : null}
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{description}</AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel disabled={pending}>{cancelLabel ?? t('common.actions.cancel')}</AlertDialogCancel>
          <AlertDialogAction
            variant={tone === 'danger' ? 'destructive' : 'default'}
            disabled={pending}
            onClick={() => void confirm()}
          >
            {pending ? <Spinner /> : null}
            {confirmLabel}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  )
}
