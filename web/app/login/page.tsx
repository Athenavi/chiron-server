'use client'

import { useEffect, useRef, useState } from 'react'
import { useRouter } from 'next/navigation'
import {
  Alert,
  Button,
  Card,
  DangerIcon,
  Description,
  InfoIcon,
  Input,
  Label,
  Spinner,
  TextField,
  toast,
} from '@heroui/react'
import { useAuth } from '@/components/auth/AuthProvider'
import { LocaleSwitcher } from '@/components/theme/LocaleSwitcher'
import { ThemeSwitcher } from '@/components/theme/ThemeSwitcher'
import { ChironMark, EyeIcon, EyeOffIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import { api, isMockMode } from '@/lib/api'
import { ApiError } from '@/lib/types'

type Translate = (key: string, vars?: Record<string, string | number>) => string

/** 后端稳定错误码 → i18n key（见 chiron-server `src/error.rs`）。 */
const AUTH_ERROR_KEYS: Record<string, string> = {
  'auth.required': 'auth.errors.invalidCredentials',
  'auth.locked': 'auth.errors.locked',
  'auth.password_change_required': 'auth.errors.passwordChangeRequired',
  bad_request: 'auth.errors.badRequest',
  forbidden: 'auth.errors.forbidden',
  internal_error: 'auth.errors.internal',
  network_error: 'auth.errors.network',
}

function loginErrorMessage(t: Translate, error: ApiError): string {
  if (error.code === 'auth.locked') {
    const wait = /retry in (\d+)s/.exec(error.message)?.[1]
    if (wait) return t('auth.errors.lockedRetry', { seconds: wait })
  }
  const mapped = AUTH_ERROR_KEYS[error.code]
  if (mapped) return t(mapped)
  if (error.status === 401) return t('auth.errors.invalidCredentials')
  return error.message
}

/**
 * 登录页。
 * 已登录访问时直接跳 `/sessions`（必须改密则跳 `/change-password`）。
 */
export default function LoginPage() {
  const router = useRouter()
  const { t } = useI18n()
  const { user, loading, setAuth } = useAuth()

  const [identifier, setIdentifier] = useState('')
  const [password, setPassword] = useState('')
  const [showPassword, setShowPassword] = useState(false)
  const [pending, setPending] = useState(false)
  const [fieldErrors, setFieldErrors] = useState<{ identifier?: string; password?: string }>({})
  const [formError, setFormError] = useState<string | null>(null)
  // 刚提交过登录：由提交回调负责跳转（保留 ?next=），避免被下面的守卫覆盖
  const submittedRef = useRef(false)

  useEffect(() => {
    if (loading || !user || submittedRef.current) return
    router.replace(user.must_change_password ? '/change-password' : '/sessions')
  }, [loading, user, router])

  function nextPath(): string {
    if (typeof window === 'undefined') return '/sessions'
    const raw = new URLSearchParams(window.location.search).get('next')
    if (!raw || !raw.startsWith('/') || raw.startsWith('//')) return '/sessions'
    return raw
  }

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    setFormError(null)

    const nextErrors: { identifier?: string; password?: string } = {}
    if (!identifier.trim()) nextErrors.identifier = t('auth.identifierRequired')
    if (!password) nextErrors.password = t('auth.passwordRequired')
    setFieldErrors(nextErrors)
    if (nextErrors.identifier || nextErrors.password) return

    setPending(true)
    try {
      const result = await api.login({ identifier: identifier.trim(), password })
      submittedRef.current = true
      setAuth({ user: result.user, plan: result.plan })
      toast.success(t('auth.loginSuccess'))
      router.replace(result.user.must_change_password ? '/change-password' : nextPath())
    } catch (cause) {
      setFormError(loginErrorMessage(t, ApiError.from(cause, t('auth.errors.fallback'))))
    } finally {
      setPending(false)
    }
  }

  function fillDemo() {
    setIdentifier('demo@example.com')
    setPassword('Demo!2026')
    setFieldErrors({})
    setFormError(null)
  }

  const mock = isMockMode()

  return (
    <div className="grid min-h-dvh md:grid-cols-[minmax(0,1fr)_minmax(0,1.05fr)]">
      {/* 品牌区（桌面） */}
      <aside
        aria-label="Chiron"
        className="hidden flex-col justify-between bg-sidebar p-10 text-sidebar-foreground md:flex"
      >
        <div className="flex items-center gap-2 text-[15px] font-semibold">
          <ChironMark size={22} />
          Chiron
        </div>
        <div className="max-w-sm">
          <h2 className="text-2xl font-semibold leading-snug tracking-tight">
            {t('auth.heroTitle')}
          </h2>
          <p className="mt-3 text-sm leading-6 text-sidebar-muted">{t('auth.heroBody')}</p>
        </div>
        <p className="text-xs text-sidebar-muted">{t('auth.heroFoot')}</p>
      </aside>

      {/* 表单区 */}
      <main className="relative flex items-center justify-center px-5 py-14 md:py-16">
        <div className="absolute right-4 top-4 z-10 flex items-center gap-2">
          <LocaleSwitcher className="min-h-10 md:min-h-9" />
          <ThemeSwitcher className="min-h-10 md:min-h-9" />
        </div>

        <div className="w-full max-w-sm">
          <h1 className="sr-only">{t('auth.loginPageTitle')}</h1>

          <div className="mb-6 flex items-center gap-2 md:hidden">
            <ChironMark size={22} className="text-primary" />
            <span className="text-base font-semibold">Chiron</span>
          </div>

          <Card className="w-full">
            <Card.Header>
              <Card.Title>{t('auth.loginTitle')}</Card.Title>
              <Card.Description>{t('auth.loginSubtitle')}</Card.Description>
            </Card.Header>

            <Card.Content>
              <form onSubmit={handleSubmit} className="flex flex-col gap-4" noValidate>
                <TextField
                  fullWidth
                  isDisabled={pending}
                  isInvalid={Boolean(fieldErrors.identifier)}
                >
                  <Label>{t('auth.identifier')}</Label>
                  <Input
                    name="identifier"
                    type="text"
                    autoComplete="username"
                    placeholder={t('auth.identifierPlaceholder')}
                    value={identifier}
                    onChange={(event) => setIdentifier(event.target.value)}
                  />
                  {fieldErrors.identifier ? (
                    <Description className="text-danger">{fieldErrors.identifier}</Description>
                  ) : null}
                </TextField>

                <TextField
                  fullWidth
                  isDisabled={pending}
                  isInvalid={Boolean(fieldErrors.password)}
                >
                  <Label>{t('auth.password')}</Label>
                  <div className="relative">
                    <Input
                      className="w-full pr-12"
                      name="password"
                      type={showPassword ? 'text' : 'password'}
                      autoComplete="current-password"
                      placeholder={t('auth.passwordPlaceholder')}
                      value={password}
                      onChange={(event) => setPassword(event.target.value)}
                    />
                    <Button
                      variant="ghost"
                      isIconOnly
                      isDisabled={pending}
                      aria-label={showPassword ? t('auth.hidePassword') : t('auth.showPassword')}
                      aria-pressed={showPassword}
                      className="absolute right-0 top-1/2 h-10 w-10 -translate-y-1/2 md:h-9 md:w-9"
                      onPress={() => setShowPassword((value) => !value)}
                    >
                      {showPassword ? <EyeOffIcon size={15} /> : <EyeIcon size={15} />}
                    </Button>
                  </div>
                  {fieldErrors.password ? (
                    <Description className="text-danger">{fieldErrors.password}</Description>
                  ) : (
                    <Description>{t('auth.passwordHint')}</Description>
                  )}
                </TextField>

                {formError ? (
                  <Alert
                    status="danger"
                    role="alert"
                    aria-live="assertive"
                    aria-atomic="true"
                  >
                    <Alert.Indicator>
                      <DangerIcon aria-hidden="true" className="size-4" />
                    </Alert.Indicator>
                    <Alert.Content>
                      <Alert.Title>{t('auth.loginErrorTitle')}</Alert.Title>
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
                  {pending ? t('auth.loggingIn') : t('auth.login')}
                </Button>
              </form>
            </Card.Content>
          </Card>

          <Alert status="default" className="mt-4">
            <Alert.Indicator>
              <InfoIcon aria-hidden="true" className="size-4" />
            </Alert.Indicator>
            <Alert.Content>
              <Alert.Title>{mock ? t('auth.demoBadge') : t('auth.liveBadge')}</Alert.Title>
              <Alert.Description className="break-words">
                {mock ? (
                  <>
                    {t('auth.demoHint')}{' '}
                    <span className="break-all font-mono text-[11px] text-foreground">
                      demo@example.com / Demo!2026
                    </span>
                  </>
                ) : (
                  t('auth.liveHint')
                )}
              </Alert.Description>
              {mock ? (
                <div className="mt-2">
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-10 md:h-8"
                    onPress={fillDemo}
                  >
                    {t('auth.fillDemo')}
                  </Button>
                </div>
              ) : null}
            </Alert.Content>
          </Alert>
        </div>
      </main>
    </div>
  )
}
