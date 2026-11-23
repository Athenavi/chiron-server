'use client'

import { useEffect, useState } from 'react'
import { api, type AlipaySettings } from '@/lib/api'
import { useI18n } from '@/lib/i18n'

export default function IntegrationsPage() {
  const { t } = useI18n()
  const [settings, setSettings] = useState<AlipaySettings | null>(null)
  const [appId, setAppId] = useState('')
  const [notifyUrl, setNotifyUrl] = useState('')
  const [returnUrl, setReturnUrl] = useState('')
  const [gateway, setGateway] = useState('')
  const [privateKey, setPrivateKey] = useState('')
  const [publicKey, setPublicKey] = useState('')
  const [clearPrivate, setClearPrivate] = useState(false)
  const [clearPublic, setClearPublic] = useState(false)
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState('')
  const [error, setError] = useState('')

  async function load() {
    try {
      const value = await api.alipaySettings()
      setSettings(value)
      setAppId(value.app_id)
      setNotifyUrl(value.notify_url)
      setReturnUrl(value.return_url)
      setGateway(value.gateway)
    } catch (err) {
      if ((err as { status?: number })?.status === 401) return
      setError(err instanceof Error ? err.message : t('integrations.loadFailed'))
    }
  }

  useEffect(() => {
    void load()
  }, [])

  async function save(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError('')
    setNotice('')
    try {
      const body: Record<string, string> = {
        app_id: appId,
        notify_url: notifyUrl,
        return_url: returnUrl,
        gateway,
      }
      if (privateKey.trim()) body.private_key = privateKey
      else if (clearPrivate) body.private_key = ''
      if (publicKey.trim()) body.public_key = publicKey
      else if (clearPublic) body.public_key = ''
      const value = await api.saveAlipaySettings(body)
      setSettings(value)
      setPrivateKey('')
      setPublicKey('')
      setClearPrivate(false)
      setClearPublic(false)
      setNotice(t('integrations.saved'))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('integrations.saveFailed'))
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <h1 style={{ marginTop: 0 }}>{t('integrations.title')}</h1>
      <p className="muted">{t('integrations.subtitle')}</p>
      {settings && (
        <p className="muted">
          {settings.configured ? t('integrations.configured') : t('integrations.notConfigured')} ·{' '}
          {settings.has_private_key ? t('integrations.hasPrivate') : t('integrations.noPrivate')} ·{' '}
          {settings.has_public_key ? t('integrations.hasPublic') : t('integrations.noPublic')}
        </p>
      )}
      {settings && !settings.secrets_key_configured && (
        <p className="error">{t('integrations.secretsKeyMissing')}</p>
      )}
      {error && <p className="error">{error}</p>}
      {notice && <p className="muted">{notice}</p>}
      <div className="card" style={{ maxWidth: 760 }}>
        <h2 style={{ marginTop: 0 }}>{t('integrations.alipayTitle')}</h2>
        <form onSubmit={save}>
          <label>{t('integrations.appId')}</label>
          <input value={appId} onChange={e => setAppId(e.target.value)} />
          <label>{t('integrations.notifyUrl')}</label>
          <input value={notifyUrl} onChange={e => setNotifyUrl(e.target.value)} />
          <label>{t('integrations.returnUrl')}</label>
          <input value={returnUrl} onChange={e => setReturnUrl(e.target.value)} />
          <label>{t('integrations.gateway')}</label>
          <input value={gateway} onChange={e => setGateway(e.target.value)} />
          <label>{t('integrations.privateKey')}</label>
          <textarea
            value={privateKey}
            onChange={e => setPrivateKey(e.target.value)}
            rows={4}
            placeholder="-----BEGIN PRIVATE KEY-----"
          />
          <label className="muted" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input
              type="checkbox"
              checked={clearPrivate}
              onChange={e => setClearPrivate(e.target.checked)}
            />
            {t('integrations.clearPrivate')}
          </label>
          <label>{t('integrations.publicKey')}</label>
          <textarea
            value={publicKey}
            onChange={e => setPublicKey(e.target.value)}
            rows={4}
            placeholder="-----BEGIN PUBLIC KEY-----"
          />
          <label className="muted" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input
              type="checkbox"
              checked={clearPublic}
              onChange={e => setClearPublic(e.target.checked)}
            />
            {t('integrations.clearPublic')}
          </label>
          <p className="muted">{t('integrations.keysHint')}</p>
          <button className="primary" type="submit" disabled={busy}>
            {busy ? t('integrations.saving') : t('integrations.save')}
          </button>
        </form>
      </div>
    </>
  )
}
