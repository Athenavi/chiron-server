'use client'

/**
 * 危险操作确认弹窗（AlertDialog）：清空全部记忆、删除单条记忆共用。
 *
 * - 焦点由 HeroUI AlertDialog 管理，初始焦点落在「取消」，避免误触删除；
 *   默认不可点外部关闭、Esc 不可关（必须显式选择）；
 * - 失败信息展示在弹窗内（role="alert"），弹窗保持打开，方便直接重试；
 * - 提交中禁用两个按钮并显示 pending；按钮高度 ≥40px，便于触控。
 */

import { AlertDialog, Button } from '@heroui/react'
import { AlertIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'

export interface ConfirmDialogProps {
  open: boolean
  title: string
  description: string
  confirmLabel?: string
  cancelLabel?: string
  isPending?: boolean
  error?: string | null
  onOpenChange: (open: boolean) => void
  onConfirm: () => void
}

export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel,
  cancelLabel,
  isPending = false,
  error = null,
  onOpenChange,
  onConfirm,
}: ConfirmDialogProps) {
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
          <AlertDialog.Dialog role="alertdialog" aria-label={title}>
            <AlertDialog.Header>
              <AlertDialog.Icon status="danger">
                <AlertIcon size={18} aria-hidden="true" />
              </AlertDialog.Icon>
              <AlertDialog.Heading>{title}</AlertDialog.Heading>
            </AlertDialog.Header>

            <AlertDialog.Body className="flex flex-col gap-2">
              <p className="text-[13px] leading-6 text-muted">{description}</p>
              {error ? (
                <p role="alert" className="text-[12px] leading-5 text-danger">
                  {error}
                </p>
              ) : null}
            </AlertDialog.Body>

            <AlertDialog.Footer>
              <Button
                variant="ghost"
                className="h-10 px-4"
                isDisabled={isPending}
                onPress={() => onOpenChange(false)}
                autoFocus
              >
                {cancelLabel ?? t('common.cancel')}
              </Button>
              <Button
                variant="danger"
                className="h-10 px-4"
                isPending={isPending}
                onPress={onConfirm}
              >
                <TrashIcon size={14} aria-hidden="true" />
                {confirmLabel ?? t('common.confirm')}
              </Button>
            </AlertDialog.Footer>
          </AlertDialog.Dialog>
        </AlertDialog.Container>
      </AlertDialog.Backdrop>
    </AlertDialog>
  )
}
