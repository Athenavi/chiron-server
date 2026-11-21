'use client'

/**
 * 成员写操作的二次确认弹窗：重置密码（普通确认）与删除成员（需输入邮箱）。
 *
 * - `pending` 期间禁止关闭（点外部 / Esc 都忽略），避免请求在飞时弹窗消失；
 * - 需要输入确认文本时，确认按钮在文本完全匹配前保持禁用；初始焦点在输入框，
 *   无需输入时落在确认按钮（由 HeroUI Modal 管理焦点恢复）；
 * - 输入不匹配时用 `FieldError` 关联到输入框，读屏可感知；
 * - 按钮高度 ≥40px，360px 下仍可点按。
 */

import { useEffect, useState } from 'react'
import { Alert, Button, FieldError, Input, Label, Modal, TextField } from '@heroui/react'
import { AlertIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'

export interface ConfirmActionModalProps {
  isOpen: boolean
  title: string
  description: string
  confirmLabel: string
  /** 危险操作（删除）：确认按钮用危险色并带警告图标。 */
  danger?: boolean
  /** 需要用户逐字输入的确认文本（通常是被操作成员的邮箱）。 */
  confirmationText?: string
  pending?: boolean
  onConfirm: () => void
  onClose: () => void
}

export function ConfirmActionModal({
  isOpen,
  title,
  description,
  confirmLabel,
  danger = false,
  confirmationText,
  pending = false,
  onConfirm,
  onClose,
}: ConfirmActionModalProps) {
  const { t } = useI18n()
  const [typed, setTyped] = useState('')

  // 每次打开都清空上一次输入的确认文本
  useEffect(() => {
    if (isOpen) setTyped('')
  }, [isOpen])

  const requiresText = Boolean(confirmationText)
  const matches = !requiresText || typed.trim() === confirmationText
  const showMismatch = requiresText && typed.length > 0 && !matches

  function handleOpenChange(open: boolean) {
    if (open || pending) return
    onClose()
  }

  return (
    <Modal isOpen={isOpen} onOpenChange={handleOpenChange}>
      <Modal.Backdrop isDismissable={!pending}>
        <Modal.Container placement="center" scroll="inside">
          <Modal.Dialog
            role={danger ? 'alertdialog' : 'dialog'}
            aria-label={title}
            className="w-full max-w-md"
          >
            <Modal.Header>
              {danger ? (
                <Modal.Icon className="bg-danger-soft text-danger">
                  <AlertIcon size={18} aria-hidden="true" />
                </Modal.Icon>
              ) : null}
              <div className="min-w-0 flex-1">
                <Modal.Heading>{title}</Modal.Heading>
                <p className="mt-0.5 text-xs leading-5 text-muted">{description}</p>
              </div>
            </Modal.Header>

            <Modal.Body className="flex flex-col gap-3">
              {danger ? (
                <Alert status="warning">
                  <Alert.Indicator>
                    <AlertIcon aria-hidden="true" className="size-4" />
                  </Alert.Indicator>
                  <Alert.Content>
                    <Alert.Title>{t('members.confirm.irreversibleTitle')}</Alert.Title>
                    <Alert.Description>
                      {t('members.confirm.irreversibleDescription')}
                    </Alert.Description>
                  </Alert.Content>
                </Alert>
              ) : null}

              {requiresText ? (
                <TextField
                  fullWidth
                  isDisabled={pending}
                  isInvalid={showMismatch}
                  value={typed}
                  onChange={(value) => setTyped(value)}
                >
                  <Label>{t('members.confirm.typeToConfirm', { text: confirmationText ?? '' })}</Label>
                  <Input
                    autoFocus
                    autoComplete="off"
                    spellCheck={false}
                    placeholder={confirmationText}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter' && matches && !pending) {
                        event.preventDefault()
                        onConfirm()
                      }
                    }}
                  />
                  {showMismatch ? (
                    <FieldError className="text-danger">
                      {t('members.confirm.mismatch')}
                    </FieldError>
                  ) : null}
                </TextField>
              ) : null}
            </Modal.Body>

            <Modal.Footer>
              <Button
                variant="ghost"
                className="h-10 px-4"
                isDisabled={pending}
                onPress={onClose}
              >
                {t('common.cancel')}
              </Button>
              <Button
                variant={danger ? 'danger' : 'primary'}
                className="h-10 px-4"
                isPending={pending}
                isDisabled={!matches}
                onPress={onConfirm}
                autoFocus={!requiresText}
              >
                {confirmLabel}
              </Button>
            </Modal.Footer>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  )
}
