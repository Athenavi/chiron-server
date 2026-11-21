'use client'

import { useEffect, useRef, useState } from 'react'
import { useRouter } from 'next/navigation'
import { api, setToken } from '@/lib/api'
import { useI18n } from '@/lib/i18n'

function errorCode(err: unknown): string {
  return (err as { code?: string } | null)?.code || ''
}

export default function LoginPage() {
  const router = useRouter()
  const { t } = useI18n()
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [totpCode, setTotpCode] = useState('')
  const [totpRequired, setTotpRequired] = useState(false)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const totpRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (totpRequired) totpRef.current?.focus()
  }, [totpRequired])

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    const code = totpCode.trim()
    if (totpRequired && !code) {
      setError(t('login.codeRequired'))
      totpRef.current?.focus()
      return
    }
    setBusy(true)
    setError('')
    try {
      const result = await api.login(email.trim(), password, totpRequired ? code : undefined)
      setToken(result.token)
      router.replace('/tenants')
    } catch (err) {
      const kind = errorCode(err)
      if (kind === 'auth.totp_required') {
        setTotpRequired(true)
        setError(t('login.totpRequired'))
      } else if (kind === 'auth.totp_invalid') {
        setTotpRequired(true)
        setTotpCode('')
        setError(t('common.totpInvalid'))
      } else {
        setError(err instanceof Error ? err.message : t('login.failed'))
      }
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="card" style={{ maxWidth: 420, margin: '48px auto' }}>
      <h1 style={{ marginTop: 0 }}>{t('login.title')}</h1>
      <p className="muted">{t('login.subtitle')}</p>
      <form onSubmit={submit}>
        <label>{t('login.email')}</label>
        <input value={email} onChange={e => setEmail(e.target.value)} autoComplete="username" />
        <label>{t('login.password')}</label>
        <input
          type="password"
          value={password}
          onChange={e => setPassword(e.target.value)}
          autoComplete="current-password"
        />
        {totpRequired && (
          <>
            <label>{t('login.totpLabel')}</label>
            <input
              ref={totpRef}
              value={totpCode}
              onChange={e => setTotpCode(e.target.value.trimStart().slice(0, 12))}
              inputMode="text"
              autoComplete="one-time-code"
              maxLength={12}
              placeholder={t('login.totpPlaceholder')}
            />
          </>
        )}
        <div style={{ marginTop: 18 }}>
          <button className="primary" disabled={busy} type="submit">
            {busy ? t('login.submitting') : t('login.submit')}
          </button>
        </div>
      </form>
      {error && <p className="error">{error}</p>}
    </div>
  )
}
