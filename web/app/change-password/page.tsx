'use client'

import { useEffect, useState } from 'react'
import { useRouter } from 'next/navigation'
import {
  Alert,
  Button,
  Card,
  DangerIcon,
  Description,
  Input,
  Label,
  Spinner,
  TextField,
  toast,
} from '@heroui/react'
import { useAuth } from '@/components/auth/AuthProvider'
import { LocaleSwitcher } from '@/components/theme/LocaleSwitcher'
import { ThemeSwitcher } from '@/components/theme/ThemeSwitcher'
import { ChironMark } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import { api } from '@/lib/api'
import { ApiError } from '@/lib/types'

type Translate = (key: string, vars?: Record<string, string | number>) => string

/** 后端稳定错误码 → i18n key（见 chiron-server `src/error.rs`）。 */
const CHANGE_PASSWORD_ERROR_KEYS: Record<string, string> = {
  'auth.required': 'auth.errors.sessionExpired',
  bad_request: 'auth.errors.weakPassword',
  forbidden: 'auth.errors.forbiddenChange',
  network_error: 'auth.errors.network',
  internal_error: 'auth.errors.internal',
}

function changePasswordErrorMessage(t: Translate, error: ApiError): string {
  const key = CHANGE_PASSWORD_ERROR_KEYS[error.code]
  return key ? t(key) : error.message
}

/** 首次登录强制改密 / 主动改密。成功后进入 `/sessions`。 */
export default function ChangePasswordPage() {
  const router = useRouter()
  const { t } = useI18n()
  const { user, plan, loading, setAuth } = useAuth()

  const [oldPassword, setOldPassword] = useState('')
  const [newPassword, setNewPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [pending, setPending] = useState(false)
  const [fieldErrors, setFieldErrors] = useState<{
    oldPassword?: string
    newPassword?: string
    confirm?: string
  }>({})
  const [formError, setFormError] = useState<string | null>(null)

  useEffect(() => {
    if (loading) return
    if (!user) router.replace('/login')
  }, [loading, user, router])

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setFormError(null)

    const nextErrors: { oldPassword?: string; newPassword?: string; confirm?: string } = {}
    if (!oldPassword) nextErrors.oldPassword = t('auth.oldPasswordRequired')
    if (newPassword.length < 8) nextErrors.newPassword = t('auth.tooShort')
    else if (newPassword === oldPassword) nextErrors.newPassword = t('auth.sameAsOld')
    if (newPassword !== confirm) nextErrors.confirm = t('auth.mismatch')
    setFieldErrors(nextErrors)
    if (nextErrors.oldPassword || nextErrors.newPassword || nextErrors.confirm) return

    setPending(true)
    try {
      const result = await api.changePassword(oldPassword, newPassword)
      setAuth({ user: result.user, plan })
      toast.success(t('auth.changed'))
      router.replace('/sessions')
    } catch (cause) {
      setFormError(
        changePasswordErrorMessage(t, ApiError.from(cause, t('auth.changeFailed'))),
      )
    } finally {
      setPending(false)
    }
  }

  if (loading || !user) {
    return (
      <div className="flex min-h-dvh items-center justify-center bg-background">
        <Spinner size="lg" color="accent" aria-label={t('auth.checkingSession')} />
      </div>
    )
  }

  const firstLogin = user.must_change_password
  // 提交后的错误优先；输入过程中也实时提示两次输入不一致
  const mismatch = Boolean(fieldErrors.confirm) || (Boolean(confirm) && confirm !== newPassword)

  return (
    <main className="relative flex min-h-dvh items-center justify-center bg-background px-5 py-14 md:py-16">
      <div className="absolute right-4 top-4 z-10 flex items-center gap-2">
        <LocaleSwitcher className="min-h-10 md:min-h-9" />
        <ThemeSwitcher className="min-h-10 md:min-h-9" />
      </div>

      <div className="w-full max-w-sm">
        <h1 className="sr-only">{firstLogin ? t('auth.firstLoginTitle') : t('auth.changeTitle')}</h1>

        <div className="mb-6 flex items-center gap-2">
          <ChironMark size={22} className="text-primary" />
          <span className="text-base font-semibold">Chiron</span>
        </div>

        <Card className="w-full">
          <Card.Header>
            <Card.Title>{firstLogin ? t('auth.firstLoginTitle') : t('auth.changeTitle')}</Card.Title>
            <Card.Description>
              {firstLogin
                ? t('auth.firstLoginSubtitle')
                : t('auth.changeSubtitle', { email: user.email })}
            </Card.Description>
          </Card.Header>

          <Card.Content>
            <form onSubmit={handleSubmit} className="flex flex-col gap-4" noValidate>
              <TextField
                fullWidth
                isDisabled={pending}
                isInvalid={Boolean(fieldErrors.oldPassword)}
              >
                <Label>{t('auth.oldPassword')}</Label>
                <Input
                  name="current-password"
                  type="password"
                  autoComplete="current-password"
                  value={oldPassword}
                  onChange={(event) => setOldPassword(event.target.value)}
                />
                {fieldErrors.oldPassword ? (
                  <Description className="text-danger">{fieldErrors.oldPassword}</Description>
                ) : null}
              </TextField>

              <TextField
                fullWidth
                isDisabled={pending}
                isInvalid={Boolean(fieldErrors.newPassword)}
              >
                <Label>{t('auth.newPassword')}</Label>
                <Input
                  name="new-password"
                  type="password"
                  autoComplete="new-password"
                  placeholder={t('auth.newPasswordPlaceholder')}
                  value={newPassword}
                  onChange={(event) => setNewPassword(event.target.value)}
                />
                {fieldErrors.newPassword ? (
                  <Description className="text-danger">{fieldErrors.newPassword}</Description>
                ) : (
                  <Description>{t('auth.newPasswordHint')}</Description>
                )}
              </TextField>

              <TextField fullWidth isDisabled={pending} isInvalid={mismatch}>
                <Label>{t('auth.confirmPassword')}</Label>
                <Input
                  name="confirm-password"
                  type="password"
                  autoComplete="new-password"
                  value={confirm}
                  onChange={(event) => setConfirm(event.target.value)}
                />
                {mismatch ? (
                  <Description className="text-danger">
                    {fieldErrors.confirm ?? t('auth.mismatch')}
                  </Description>
                ) : null}
              </TextField>

              {formError ? (
                <Alert status="danger" role="alert" aria-live="assertive" aria-atomic="true">
                  <Alert.Indicator>
                    <DangerIcon aria-hidden="true" className="size-4" />
                  </Alert.Indicator>
                  <Alert.Content>
                    <Alert.Title>{t('auth.saveErrorTitle')}</Alert.Title>
                    <Alert.Description className="break-words">{formError}</Alert.Description>
                  </Alert.Content>
                </Alert>
              ) : null}

              <Button
                type="submit"
                variant="primary"
                fullWidth
                isPending={pending}
                className="mt-1 h-11 md:h-10"
              >
                {pending ? <Spinner size="sm" color="current" /> : null}
                {pending ? t('auth.changing') : t('auth.submitChange')}
              </Button>
            </form>
          </Card.Content>
        </Card>
      </div>
    </main>
  )
}
