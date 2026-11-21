'use client'

import { useCallback, useEffect, useState } from 'react'
import Link from 'next/link'
import { useRouter } from 'next/navigation'
import { api, setToken } from '@/lib/api'
import { useI18n } from '@/lib/i18n'

type Me = { id: string; email: string; totp_enabled: boolean }

export default function Home() {
  const router = useRouter()
  const { t } = useI18n()
  const [me, setMe] = useState<Me | null>(null)
  const [secret, setSecret] = useState('')
  const [otpauth, setOtpauth] = useState('')
  const [recoveryCodes, setRecoveryCodes] = useState<string[]>([])
  const [code, setCode] = useState('')
  const [notice, setNotice] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  const load = useCallback(async () => {
    try {
      setMe(await api.me())
    } catch {
      setToken('')
      router.replace('/login')
    }
  }, [router])

  useEffect(() => {
    // 生产使用 httpOnly cookie，不能靠 sessionStorage 判断登录态；/me 的 401 会跳登录。
    void load()
  }, [load])

  function fail(err: unknown) {
    setNotice('')
    const kind = (err as { code?: string } | null)?.code
    if (kind === 'auth.totp_invalid') setError(t('common.totpInvalid'))
    else setError(err instanceof Error ? err.message : t('common.operationFailed'))
  }

  async function beginSetup() {
    setBusy(true)
    setError('')
    setNotice('')
    try {
      const setup = await api.totpSetup()
      setSecret(setup.secret)
      setOtpauth(setup.otpauth_url)
      setCode('')
      setNotice(t('security.secretNotice'))
    } catch (err) {
      fail(err)
    } finally {
      setBusy(false)
    }
  }

  async function enable() {
    setBusy(true)
    setError('')
    setNotice('')
    try {
      const result = await api.totpEnable(code.trim())
      setSecret('')
      setOtpauth('')
      setCode('')
      setRecoveryCodes(result.recovery_codes || [])
      setNotice(t('security.enabledNotice'))
      await load()
    } catch (err) {
      fail(err)
    } finally {
      setBusy(false)
    }
  }

  async function disable() {
    setBusy(true)
    setError('')
    setNotice('')
    try {
      await api.totpDisable(code.trim())
      setCode('')
      setNotice(t('security.disabledNotice'))
      await load()
    } catch (err) {
      fail(err)
    } finally {
      setBusy(false)
    }
  }

  if (!me) return <p className="muted">{t('common.loading')}</p>

  return (
    <div className="card" style={{ maxWidth: 620, margin: '24px auto' }}>
      <h1 style={{ marginTop: 0 }}>{t('security.title')}</h1>
      <p className="muted">
        {me.email} · {t('security.totpLabel')}
        <strong>{me.totp_enabled ? t('security.enabled') : t('security.disabled')}</strong>
      </p>

      {!me.totp_enabled && !secret && (
        <button className="primary" disabled={busy} onClick={beginSetup} type="button">
          {busy ? t('common.processing') : t('security.enable')}
        </button>
      )}

      {!me.totp_enabled && secret && (
        <div>
          <p className="muted">{t('security.secretLabel')}</p>
          <p>
            <code>{secret}</code>
          </p>
          <p className="muted" style={{ wordBreak: 'break-all' }}>
            {otpauth}
          </p>
          <label>{t('security.codeLabel')}</label>
          <input
            value={code}
            onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
            inputMode="numeric"
            autoComplete="one-time-code"
            maxLength={6}
          />
          <div style={{ marginTop: 12 }}>
            <button className="primary" disabled={busy || code.length !== 6} onClick={enable} type="button">
              {t('security.confirmEnable')}
            </button>
          </div>
        </div>
      )}

      {me.totp_enabled && (
        <div>
          <label>{t('security.currentCodeLabel')}</label>
          <input
            value={code}
            onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
            inputMode="numeric"
            autoComplete="one-time-code"
            maxLength={6}
          />
          <div style={{ marginTop: 12 }}>
            <button disabled={busy || code.length !== 6} onClick={disable} type="button">
              {t('security.disable')}
            </button>
          </div>
        </div>
      )}

      {recoveryCodes.length > 0 && (
        <div style={{ marginTop: 12 }}>
          <p className="muted">{t('security.recoveryLabel')}</p>
          <pre style={{ background: '#111', color: '#eee', padding: 12, borderRadius: 6 }}>
            {recoveryCodes.join('\n')}
          </pre>
          <button type="button" onClick={() => setRecoveryCodes([])}>
            {t('security.recoveryHide')}
          </button>
        </div>
      )}

      {notice && <p className="muted">{notice}</p>}
      {error && <p className="error">{error}</p>}
      <p style={{ marginTop: 20 }}>
        <Link href="/tenants">{t('security.goTenants')}</Link>
      </p>
    </div>
  )
}
