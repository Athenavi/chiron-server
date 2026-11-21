'use client'

/**
 * 删除归档会话的确认弹窗（AlertDialog）。
 *
 * - 不可点外部关闭、Esc 不可关，必须显式选择；
 * - 失败信息留在弹窗内，方便直接重试；
 * - 提交中两个按钮都禁用并显示 pending；
 * - 文案走 `archived.dialog.*` / `common.cancel` i18n。
 */

import { AlertDialog, Button } from '@heroui/react'
import { AlertIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'

export interface DeleteSessionDialogProps {
  open: boolean
  /** 待删除会话的展示标题。 */
  title: string
  isPending?: boolean
  error?: string | null
  onOpenChange: (open: boolean) => void
  onConfirm: () => void
}

export function DeleteSessionDialog({
  open,
  title,
  isPending = false,
  error = null,
  onOpenChange,
  onConfirm,
}: DeleteSessionDialogProps) {
  const { t } = useI18n()

  return (
    <AlertDialog
      isOpen={open}
      onOpenChange={(next) => {
        if (isPending && !next) return
        onOpenChange(next)
      }}
    >
      <AlertDialog.Backdrop isDismissable={false}>
        <AlertDialog.Container placement="center">
          <AlertDialog.Dialog role="alertdialog" aria-label={t('archived.dialog.label')}>
            <AlertDialog.Header>
              <AlertDialog.Icon status="danger">
                <AlertIcon size={18} />
              </AlertDialog.Icon>
              <AlertDialog.Heading>{t('archived.dialog.heading')}</AlertDialog.Heading>
            </AlertDialog.Header>

            <AlertDialog.Body className="flex flex-col gap-2">
              <p className="text-[13px] leading-6 text-muted">
                {t('archived.dialog.body', { title })}
              </p>
              {error ? (
                <p role="alert" className="text-[12px] leading-5 text-danger">
                  {error}
                </p>
              ) : null}
            </AlertDialog.Body>

            <AlertDialog.Footer>
              <Button variant="ghost" isDisabled={isPending} onPress={() => onOpenChange(false)}>
                {t('common.cancel')}
              </Button>
              <Button variant="danger" isPending={isPending} onPress={onConfirm} autoFocus>
                <TrashIcon size={14} />
                {t('archived.delete')}
              </Button>
            </AlertDialog.Footer>
          </AlertDialog.Dialog>
        </AlertDialog.Container>
      </AlertDialog.Backdrop>
    </AlertDialog>
  )
}
