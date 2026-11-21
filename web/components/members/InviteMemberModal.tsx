'use client'

/**
 * 邀请成员弹窗。
 *
 * - 邮箱必填并做格式校验；用户名 / 昵称可选（留空由服务端按邮箱派生）；
 * - 角色用 Select：默认 user；admin 可邀请；owner 仅当当前账号是 owner 时出现；
 * - 初始密码可选，填写时要求 ≥ 8 位，留空由服务端生成；
 * - 创建成功后把结果交给页面：页面负责展示一次性密码并刷新列表；
 * - 失败保留弹窗并同时 toast + 内联 Alert，方便修正后重试；
 * - 字段错误用 `FieldError` 与输入框关联；双列字段在 <640px 自动塌陷为单列。
 */

import { useEffect, useState } from 'react'
import {
  Alert,
  Button,
  Description,
  FieldError,
  Input,
  Label,
  ListBox,
  Modal,
  Select,
  TextField,
  toast,
} from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError, type CreateUserResult } from '@/lib/types'
import { AlertIcon, PlusIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'

type InviteRole = 'user' | 'admin' | 'owner'

const EMAIL_PATTERN = /^[^\s@]+@[^\s@]+\.[^\s@]+$/

const ROLE_VALUES: InviteRole[] = ['user', 'admin', 'owner']

export interface InviteMemberModalProps {
  isOpen: boolean
  /** 当前账号是 owner 才能授予 owner 角色。 */
  canInviteOwner: boolean
  onOpenChange: (open: boolean) => void
  onInvited: (result: CreateUserResult) => void
}

export function InviteMemberModal({
  isOpen,
  canInviteOwner,
  onOpenChange,
  onInvited,
}: InviteMemberModalProps) {
  const { t } = useI18n()
  const [email, setEmail] = useState('')
  const [username, setUsername] = useState('')
  const [nickname, setNickname] = useState('')
  const [role, setRole] = useState<InviteRole>('user')
  const [initialPassword, setInitialPassword] = useState('')
  const [emailError, setEmailError] = useState<string | null>(null)
  const [passwordError, setPasswordError] = useState<string | null>(null)
  const [formError, setFormError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  // 每次打开都重置表单（也覆盖「上次失败后关闭再打开」的情况）
  useEffect(() => {
    if (!isOpen) return
    setEmail('')
    setUsername('')
    setNickname('')
    setRole('user')
    setInitialPassword('')
    setEmailError(null)
    setPasswordError(null)
    setFormError(null)
    setSubmitting(false)
  }, [isOpen])

  // 权限变化时（极少数：账号角色被降级）把无效的 owner 选择收回到 user
  useEffect(() => {
    if (!canInviteOwner && role === 'owner') setRole('user')
  }, [canInviteOwner, role])

  const roleValues = canInviteOwner
    ? ROLE_VALUES
    : ROLE_VALUES.filter((value) => value !== 'owner')
  const roleOptions = roleValues.map((value) => ({
    value,
    label: t(`members.role.${value}`),
  }))

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (submitting) return

    const trimmedEmail = email.trim()
    if (!trimmedEmail) {
      setEmailError(t('members.invite.emailRequired'))
      return
    }
    if (!EMAIL_PATTERN.test(trimmedEmail)) {
      setEmailError(t('members.invite.emailInvalid'))
      return
    }

    const password = initialPassword.trim()
    if (password && password.length < 8) {
      setPasswordError(t('members.invite.initialPasswordTooShort'))
      return
    }

    setEmailError(null)
    setPasswordError(null)
    setFormError(null)
    setSubmitting(true)
    try {
      const result = await api.createUser({
        email: trimmedEmail,
        username: username.trim() || undefined,
        nickname: nickname.trim() || undefined,
        role,
        initial_password: password || undefined,
      })
      toast.success(t('members.toast.memberCreated'))
      onInvited(result)
    } catch (cause) {
      const message = ApiError.from(cause, t('members.error.invite')).message
      setFormError(message)
      toast.danger(message)
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <Modal
      isOpen={isOpen}
      onOpenChange={(open) => {
        if (submitting) return
        onOpenChange(open)
      }}
    >
      <Modal.Backdrop isDismissable={!submitting}>
        <Modal.Container placement="center" scroll="inside">
          <Modal.Dialog aria-label={t('members.invite.title')} className="w-full max-w-lg">
            <Modal.Header>
              <Modal.Icon className="bg-accent-soft text-accent">
                <PlusIcon size={18} aria-hidden="true" />
              </Modal.Icon>
              <div className="min-w-0 flex-1">
                <Modal.Heading>{t('members.invite.title')}</Modal.Heading>
                <p className="mt-0.5 text-xs leading-5 text-muted">{t('members.invite.hint')}</p>
              </div>
            </Modal.Header>

            <Modal.Body>
              <form
                id="invite-member-form"
                className="flex flex-col gap-4"
                noValidate
                onSubmit={(event) => void handleSubmit(event)}
              >
                <TextField
                  fullWidth
                  isRequired
                  isDisabled={submitting}
                  isInvalid={Boolean(emailError)}
                  value={email}
                  onChange={(value) => setEmail(value)}
                >
                  <Label>{t('members.invite.email')}</Label>
                  <Input
                    autoFocus
                    type="email"
                    autoComplete="email"
                    placeholder={t('members.invite.emailPlaceholder')}
                  />
                  {emailError ? (
                    <FieldError className="text-danger">{emailError}</FieldError>
                  ) : (
                    <Description>{t('members.invite.emailDescription')}</Description>
                  )}
                </TextField>

                <div className="grid gap-4 sm:grid-cols-2">
                  <TextField
                    fullWidth
                    isDisabled={submitting}
                    value={username}
                    onChange={(value) => setUsername(value)}
                  >
                    <Label>{t('members.invite.username')}</Label>
                    <Input
                      autoComplete="off"
                      placeholder={t('members.invite.usernamePlaceholder')}
                    />
                  </TextField>

                  <TextField
                    fullWidth
                    isDisabled={submitting}
                    value={nickname}
                    onChange={(value) => setNickname(value)}
                  >
                    <Label>{t('members.invite.nickname')}</Label>
                    <Input
                      autoComplete="nickname"
                      placeholder={t('members.invite.nicknamePlaceholder')}
                    />
                  </TextField>
                </div>

                <div className="flex flex-col gap-1.5">
                  <Select.Root
                    isDisabled={submitting}
                    selectedKey={role}
                    onSelectionChange={(key) => {
                      if (key !== null) setRole(String(key) as InviteRole)
                    }}
                  >
                    <Label>{t('members.invite.role')}</Label>
                    <Select.Trigger>
                      <Select.Value />
                      <Select.Indicator />
                    </Select.Trigger>
                    <Select.Popover>
                      <ListBox>
                        {roleOptions.map((option) => (
                          <ListBox.Item
                            key={option.value}
                            id={option.value}
                            textValue={option.label}
                          >
                            {option.label}
                            <ListBox.Item.Indicator />
                          </ListBox.Item>
                        ))}
                      </ListBox>
                    </Select.Popover>
                  </Select.Root>
                  <p className="text-xs leading-5 text-muted">
                    {t('members.invite.roleHintAdmin')}
                    {canInviteOwner
                      ? t('members.invite.roleHintOwnerGrant')
                      : t('members.invite.roleHintOwnerOnly')}
                  </p>
                </div>

                <TextField
                  fullWidth
                  isDisabled={submitting}
                  isInvalid={Boolean(passwordError)}
                  value={initialPassword}
                  onChange={(value) => setInitialPassword(value)}
                >
                  <Label>{t('members.invite.initialPassword')}</Label>
                  <Input
                    type="password"
                    autoComplete="new-password"
                    placeholder={t('members.invite.initialPasswordPlaceholder')}
                  />
                  {passwordError ? (
                    <FieldError className="text-danger">{passwordError}</FieldError>
                  ) : (
                    <Description>{t('members.invite.initialPasswordDescription')}</Description>
                  )}
                </TextField>

                {formError ? (
                  <Alert status="danger">
                    <Alert.Indicator>
                      <AlertIcon aria-hidden="true" className="size-4" />
                    </Alert.Indicator>
                    <Alert.Content>
                      <Alert.Title>{t('members.invite.errorTitle')}</Alert.Title>
                      <Alert.Description>{formError}</Alert.Description>
                    </Alert.Content>
                  </Alert>
                ) : null}
              </form>
            </Modal.Body>

            <Modal.Footer>
              <Button
                variant="ghost"
                className="h-10 px-4"
                isDisabled={submitting}
                onPress={() => onOpenChange(false)}
              >
                {t('members.invite.cancel')}
              </Button>
              <Button
                type="submit"
                form="invite-member-form"
                variant="primary"
                className="h-10 px-4"
                isPending={submitting}
              >
                {t('members.invite.submit')}
              </Button>
            </Modal.Footer>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  )
}
