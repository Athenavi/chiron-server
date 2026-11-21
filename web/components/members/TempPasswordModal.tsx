'use client'

/**
 * 一次性初始 / 临时密码展示弹窗。
 *
 * - 密码只在创建成员或重置密码成功后展示一次，关闭即清空（由页面控制）；
 * - 提供复制按钮，复制失败时 toast 提示手动选择；复制结果用 `aria-live` 播报；
 * - 使用 `select-all` + 等宽字体，便于键盘用户手动复制；密码过长时换行不撑破弹窗；
 * - 关闭按钮触控目标 ≥40px。
 */

import { useEffect, useState } from 'react'
import { Button, Modal, toast } from '@heroui/react'
import { AlertIcon, CheckIcon, CopyIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'

export interface TempPasswordModalProps {
  isOpen: boolean
  title: string
  /** 密码所属成员的邮箱（副标题）。 */
  email: string
  password: string
  description: string
  onClose: () => void
}

export function TempPasswordModal({
  isOpen,
  title,
  email,
  password,
  description,
  onClose,
}: TempPasswordModalProps) {
  const { t } = useI18n()
  const [copied, setCopied] = useState(false)

  useEffect(() => {
    setCopied(false)
  }, [isOpen, password])

  async function copyPassword() {
    if (!password) return
    try {
      await navigator.clipboard.writeText(password)
      setCopied(true)
      toast.success(t('members.toast.passwordCopied'))
    } catch {
      toast.danger(t('members.toast.copyFailed'))
    }
  }

  return (
    <Modal
      isOpen={isOpen}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <Modal.Backdrop>
        <Modal.Container placement="center" scroll="inside">
          <Modal.Dialog aria-label={title} className="w-full max-w-md">
            <Modal.Header>
              <Modal.Icon className="bg-warning-soft text-warning">
                <AlertIcon size={18} aria-hidden="true" />
              </Modal.Icon>
              <div className="min-w-0 flex-1">
                <Modal.Heading>{title}</Modal.Heading>
                <p className="mt-0.5 truncate text-xs text-muted" title={email}>
                  {email}
                </p>
              </div>
            </Modal.Header>

            <Modal.Body className="flex flex-col gap-3">
              <p className="text-[13px] leading-6 text-muted">{description}</p>
              <div className="flex items-center gap-2 rounded-lg border border-border bg-surface-secondary px-3 py-2">
                <code className="min-w-0 flex-1 select-all break-all font-mono text-sm text-foreground">
                  {password || t('members.secret.noPassword')}
                </code>
                <Button
                  size="sm"
                  variant="outline"
                  className="h-10 shrink-0 gap-1.5 px-3"
                  isDisabled={!password}
                  aria-label={t('members.secret.copyAria')}
                  onPress={() => void copyPassword()}
                >
                  {copied ? (
                    <CheckIcon size={14} aria-hidden="true" />
                  ) : (
                    <CopyIcon size={14} aria-hidden="true" />
                  )}
                  {copied ? t('members.secret.copied') : t('members.secret.copy')}
                </Button>
              </div>
              <p aria-live="polite" className="sr-only">
                {copied ? t('members.toast.passwordCopied') : ''}
              </p>
              <p className="text-[11px] leading-5 text-subtle">
                {t('members.secret.closeHint')}
              </p>
            </Modal.Body>

            <Modal.Footer>
              <Button variant="primary" className="h-10 px-4" onPress={onClose} autoFocus>
                {t('members.secret.done')}
              </Button>
            </Modal.Footer>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  )
}
