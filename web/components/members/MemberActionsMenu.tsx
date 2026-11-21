'use client'

/**
 * 成员行操作菜单（Dropdown）。
 *
 * - 禁用中的成员显示「启用」，否则显示「禁用」；
 * - 删除用危险色，真正的二次确认由页面里的 ConfirmActionModal 负责；
 * - 自己也在菜单里（后端对「禁用自己 / 删除自己 / 最后一个 owner」有保护），
 *   这里只做文案提示（「（自己）」后缀），错误消息统一由页面 toast；
 * - 触发器触控目标 40px（md 以上收紧到 32px），aria-label 走 i18n。
 */

import type { Key } from 'react'
import { Dropdown } from '@heroui/react'
import type { User } from '@/lib/types'
import { CheckCircleIcon, LockIcon, LogOutIcon, MoreIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import { memberName } from './memberMeta'

export type MemberAction =
  | 'reset-password'
  | 'disable'
  | 'enable'
  | 'force-logout'
  | 'delete'

export interface MemberActionsMenuProps {
  user: User
  /** 目标是否是当前登录账号（菜单文案加「自己」提示）。 */
  isSelf: boolean
  /** 该行已有写操作在飞：菜单暂时禁用。 */
  isBusy?: boolean
  onAction: (action: MemberAction) => void
}

export function MemberActionsMenu({
  user,
  isSelf,
  isBusy = false,
  onAction,
}: MemberActionsMenuProps) {
  const { t } = useI18n()
  const name = memberName(user)
  const disabled = (user.status ?? '').toLowerCase() === 'disabled'
  const triggerLabel = t('members.menu.trigger', { name })

  const resetPassword = t('members.menu.resetPassword')
  const enableLabel = t('members.menu.enable')
  const disableLabel = t('members.menu.disable')
  const forceLogoutLabel = isSelf
    ? t('members.menu.forceLogoutSelf')
    : t('members.menu.forceLogout')
  const deleteLabel = isSelf ? t('members.menu.deleteSelf') : t('members.menu.delete')

  function handleAction(key: Key) {
    onAction(String(key) as MemberAction)
  }

  return (
    <Dropdown>
      <Dropdown.Trigger
        aria-label={triggerLabel}
        isDisabled={isBusy}
        className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted transition-colors hover:bg-surface-hover hover:text-foreground data-[focus-visible]:outline-2 data-[focus-visible]:outline-offset-1 data-[focus-visible]:outline-ring md:h-8 md:w-8"
      >
        <MoreIcon size={16} aria-hidden="true" />
      </Dropdown.Trigger>
      <Dropdown.Popover placement="bottom end">
        <Dropdown.Menu aria-label={triggerLabel} onAction={handleAction}>
          <Dropdown.Item id="reset-password" textValue={resetPassword}>
            <LockIcon size={14} aria-hidden="true" />
            <span className="truncate">{resetPassword}</span>
          </Dropdown.Item>

          {disabled ? (
            <Dropdown.Item id="enable" textValue={enableLabel}>
              <CheckCircleIcon size={14} aria-hidden="true" />
              <span className="truncate">{enableLabel}</span>
            </Dropdown.Item>
          ) : (
            <Dropdown.Item id="disable" textValue={disableLabel}>
              <LockIcon size={14} aria-hidden="true" />
              <span className="truncate">{disableLabel}</span>
            </Dropdown.Item>
          )}

          <Dropdown.Item id="force-logout" textValue={forceLogoutLabel}>
            <LogOutIcon size={14} aria-hidden="true" />
            <span className="truncate">{forceLogoutLabel}</span>
          </Dropdown.Item>

          <Dropdown.Item id="delete" textValue={deleteLabel} className="text-danger">
            <TrashIcon size={14} aria-hidden="true" />
            <span className="truncate">{deleteLabel}</span>
          </Dropdown.Item>
        </Dropdown.Menu>
      </Dropdown.Popover>
    </Dropdown>
  )
}
