'use client'

/**
 * 删除文件确认弹窗（AlertDialog）：不可点外部 / Esc 关闭，
 * 失败信息留在弹窗内，删除中的两个按钮都禁用。
 */

import { AlertDialog, Button } from '@heroui/react'
import { AlertIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import type { MediaAsset } from '@/lib/types'

export interface DeleteMediaDialogProps {
  /** 待删除文件；为空表示关闭。 */
  asset: MediaAsset | null
  isPending?: boolean
  error?: string | null
  onOpenChange: (open: boolean) => void
  onConfirm: () => void
}

export function DeleteMediaDialog({
  asset,
  isPending = false,
  error = null,
  onOpenChange,
  onConfirm,
}: DeleteMediaDialogProps) {
  const { t } = useI18n()
  const filename = asset?.filename ?? ''

  return (
    <AlertDialog
      isOpen={asset !== null}
      onOpenChange={(next) => {
        if (isPending && !next) return
        onOpenChange(next)
      }}
    >
      <AlertDialog.Backdrop isDismissable={false}>
        <AlertDialog.Container placement="center">
          <AlertDialog.Dialog
            role="alertdialog"
            aria-label={t('media.deleteTitle')}
          >
            <AlertDialog.Header>
              <AlertDialog.Icon status="danger">
                <AlertIcon size={18} />
              </AlertDialog.Icon>
              <AlertDialog.Heading>{t('media.deleteTitle')}</AlertDialog.Heading>
            </AlertDialog.Header>

            <AlertDialog.Body className="flex flex-col gap-2">
              <p className="break-words text-[13px] leading-6 text-muted">
                {t('media.deleteBody', { name: filename })}
              </p>
              {error ? (
                <p role="alert" className="text-[12px] leading-5 text-danger">
                  {error}
                </p>
              ) : null}
            </AlertDialog.Body>

            <AlertDialog.Footer>
              <Button variant="ghost" isDisabled={isPending} onPress={() => onOpenChange(false)}>
                {t('media.cancel')}
              </Button>
              <Button variant="danger" isPending={isPending} onPress={onConfirm} autoFocus>
                <TrashIcon size={14} />
                {t('media.delete')}
              </Button>
            </AlertDialog.Footer>
          </AlertDialog.Dialog>
        </AlertDialog.Container>
      </AlertDialog.Backdrop>
    </AlertDialog>
  )
}
