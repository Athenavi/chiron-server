'use client'

import { useEffect, useState } from 'react'
import { api, type AlertSettings, type MediaSettings, type ProviderEntry, type RateSettings } from '@/lib/api'
import { useI18n, type MessageKey } from '@/lib/i18n'

const RATE_LABELS: Record<'login' | 'submit' | 'tool' | 'heavy', MessageKey> = {
  login: 'settings.rate.login',
  submit: 'settings.rate.submit',
  tool: 'settings.rate.tool',
  heavy: 'settings.rate.heavy',
}

export default function SettingsPage() {
  const { t } = useI18n()
  const [alerts, setAlerts] = useState<AlertSettings | null>(null)
  const [rates, setRates] = useState<RateSettings | null>(null)
  const [webhook, setWebhook] = useState('')
  const [clearWebhook, setClearWebhook] = useState(false)
  const [cooldown, setCooldown] = useState('')
  const [activeRuns, setActiveRuns] = useState('')
  const [tenantPools, setTenantPools] = useState('')
  const [egressDenied, setEgressDenied] = useState('')
  const [rateEnabled, setRateEnabled] = useState(true)
  const [rateValues, setRateValues] = useState<Record<string, string>>({})
  const [media, setMedia] = useState<MediaSettings | null>(null)
  const [backend, setBackend] = useState('local')
  const [webdavUrl, setWebdavUrl] = useState('')
  const [webdavUser, setWebdavUser] = useState('')
  const [webdavPassword, setWebdavPassword] = useState('')
  const [webdavPrefix, setWebdavPrefix] = useState('')
  const [s3Bucket, setS3Bucket] = useState('')
  const [s3Region, setS3Region] = useState('')
  const [s3Endpoint, setS3Endpoint] = useState('')
  const [s3Access, setS3Access] = useState('')
  const [s3Secret, setS3Secret] = useState('')
  const [s3Session, setS3Session] = useState('')
  const [s3PathStyle, setS3PathStyle] = useState(false)
  const [s3Prefix, setS3Prefix] = useState('')
  const [providers, setProviders] = useState<ProviderEntry[]>([])
  const [providerId, setProviderId] = useState('openai')
  const [providerKey, setProviderKey] = useState('')
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState('')
  const [error, setError] = useState('')

  useEffect(() => {
    void (async () => {
      try {
        const [a, r] = await Promise.all([api.alertSettings(), api.rateSettings()])
        setAlerts(a)
        setCooldown(String(a.cooldown_secs))
        setActiveRuns(String(a.active_runs))
        setTenantPools(String(a.tenant_pools))
        setEgressDenied(String(a.egress_denied))
        setRates(r)
        setRateEnabled(r.enabled)
        setRateValues({
          login_burst: String(r.login.burst),
          login_per_minute: String(r.login.per_minute),
          submit_burst: String(r.submit.burst),
          submit_per_minute: String(r.submit.per_minute),
          tool_burst: String(r.tool.burst),
          tool_per_minute: String(r.tool.per_minute),
          heavy_burst: String(r.heavy.burst),
          heavy_per_minute: String(r.heavy.per_minute),
        })
        const [m, p] = await Promise.all([api.mediaSettings(), api.providerSettings()])
        setMedia(m)
        setBackend(m.backend || 'local')
        setWebdavUrl(m.webdav.url)
        setWebdavUser(m.webdav.username)
        setWebdavPrefix(m.webdav.prefix)
        setS3Bucket(m.s3.bucket)
        setS3Region(m.s3.region)
        setS3Endpoint(m.s3.endpoint ?? '')
        setS3PathStyle(Boolean(m.s3.path_style))
        setS3Prefix(m.s3.prefix ?? '')
        setProviders(p)
        setProviderId(p.find(item => item.requires_key)?.id ?? p[0]?.id ?? 'openai')
      } catch (err) {
        setError(err instanceof Error ? err.message : t('settings.loadFailed'))
      }
    })()
  }, [])

  function rateValue(key: string) {
    return rateValues[key] ?? ''
  }

  function setRate(key: string, value: string) {
    setRateValues(prev => ({ ...prev, [key]: value }))
  }

  async function saveAlerts(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError('')
    setNotice('')
    try {
      const body: Record<string, string | number> = {
        cooldown_secs: Number(cooldown) || 0,
        active_runs: Number(activeRuns) || 0,
        tenant_pools: Number(tenantPools) || 0,
        egress_denied: Number(egressDenied) || 0,
      }
      if (webhook.trim()) body.webhook_url = webhook.trim()
      else if (clearWebhook) body.webhook_url = ''
      const next = await api.saveAlertSettings(body)
      setAlerts(next)
      setWebhook('')
      setClearWebhook(false)
      setNotice(t('settings.alertsSaved'))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('settings.saveFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function saveRates(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError('')
    setNotice('')
    try {
      const body: Record<string, unknown> = { enabled: rateEnabled }
      for (const key of Object.keys(rateValues)) body[key] = Number(rateValues[key]) || 0
      const next = await api.saveRateSettings(body)
      setRates(next)
      setNotice(t('settings.ratesSaved'))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('settings.saveFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function saveMedia(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError('')
    setNotice('')
    try {
      const body: Record<string, unknown> = {
        backend,
        webdav_url: webdavUrl,
        webdav_username: webdavUser,
        webdav_prefix: webdavPrefix,
        s3_bucket: s3Bucket,
        s3_region: s3Region,
        s3_endpoint: s3Endpoint,
        s3_path_style: s3PathStyle,
        s3_prefix: s3Prefix,
      }
      if (webdavPassword.trim()) body.webdav_password = webdavPassword
      if (s3Access.trim()) body.s3_access_key = s3Access
      if (s3Secret.trim()) body.s3_secret_key = s3Secret
      if (s3Session.trim()) body.s3_session_token = s3Session
      const next = await api.saveMediaSettings(body)
      setMedia(next)
      setWebdavPassword('')
      setS3Access('')
      setS3Secret('')
      setS3Session('')
      setNotice(t('settings.mediaSaved'))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('settings.saveFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function saveProvider(event: React.FormEvent) {
    event.preventDefault()
    setBusy(true)
    setError('')
    setNotice('')
    try {
      const next = await api.saveProviderKey(providerId, providerKey)
      setProviders(next)
      setProviderKey('')
      setNotice(t('settings.providerSaved'))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('settings.saveFailed'))
    } finally {
      setBusy(false)
    }
  }

  return (
    <>
      <h1 style={{ marginTop: 0 }}>{t('settings.title')}</h1>
      <p className="muted">{t('settings.subtitle')}</p>
      {error && <p className="error">{error}</p>}
      {notice && <p className="muted">{notice}</p>}

      <div className="card" style={{ maxWidth: 760, marginBottom: 18 }}>
        <h2 style={{ marginTop: 0 }}>{t('settings.alertsTitle')}</h2>
        <p className="muted">
          {alerts?.webhook_configured ? t('settings.webhookConfigured') : t('settings.webhookMissing')}
        </p>
        <form onSubmit={saveAlerts}>
          <label>{t('settings.webhookUrl')}</label>
          <input value={webhook} onChange={e => setWebhook(e.target.value)} placeholder="https://..." />
          <label className="muted" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input type="checkbox" checked={clearWebhook} onChange={e => setClearWebhook(e.target.checked)} />
            {t('settings.clearWebhook')}
          </label>
          <label>{t('settings.cooldown')}</label>
          <input value={cooldown} onChange={e => setCooldown(e.target.value)} inputMode="numeric" />
          <label>{t('settings.activeRuns')}</label>
          <input value={activeRuns} onChange={e => setActiveRuns(e.target.value)} inputMode="numeric" />
          <label>{t('settings.tenantPools')}</label>
          <input value={tenantPools} onChange={e => setTenantPools(e.target.value)} inputMode="numeric" />
          <label>{t('settings.egressDenied')}</label>
          <input value={egressDenied} onChange={e => setEgressDenied(e.target.value)} inputMode="numeric" />
          <button className="primary" type="submit" disabled={busy}>
            {busy ? t('settings.saving') : t('settings.save')}
          </button>
        </form>
      </div>

      <div className="card" style={{ maxWidth: 760 }}>
        <h2 style={{ marginTop: 0 }}>{t('settings.ratesTitle')}</h2>
        <form onSubmit={saveRates}>
          <label className="muted" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input type="checkbox" checked={rateEnabled} onChange={e => setRateEnabled(e.target.checked)} />
            {t('settings.rateEnabled')}
          </label>
          {(['login', 'submit', 'tool', 'heavy'] as const).map(cls => (
            <div key={cls}>
              <label>{t(RATE_LABELS[cls])}</label>
              <div className="row">
                <input
                  value={rateValue(`${cls}_burst`)}
                  onChange={e => setRate(`${cls}_burst`, e.target.value)}
                  inputMode="numeric"
                  aria-label={`${cls} burst`}
                />
                <input
                  value={rateValue(`${cls}_per_minute`)}
                  onChange={e => setRate(`${cls}_per_minute`, e.target.value)}
                  inputMode="numeric"
                  aria-label={`${cls} per minute`}
                />
              </div>
            </div>
          ))}
          <p className="muted">{t('settings.ratesHint')}</p>
          <button className="primary" type="submit" disabled={busy}>
            {busy ? t('settings.saving') : t('settings.save')}
          </button>
        </form>
      </div>

      <div className="card" style={{ maxWidth: 760, marginBottom: 18 }}>
        <h2 style={{ marginTop: 0 }}>{t('settings.mediaTitle')}</h2>
        <form onSubmit={saveMedia}>
          <label>{t('settings.mediaBackend')}</label>
          <select value={backend} onChange={e => setBackend(e.target.value)}>
            <option value="local">local</option>
            <option value="webdav">webdav</option>
            <option value="s3">s3</option>
          </select>
          <p className="muted">{t('settings.webdavTitle')}</p>
          <label>URL</label>
          <input value={webdavUrl} onChange={e => setWebdavUrl(e.target.value)} placeholder="https://dav.example.com" />
          <label>{t('settings.username')}</label>
          <input value={webdavUser} onChange={e => setWebdavUser(e.target.value)} />
          <label>{t('settings.passwordKeep')}</label>
          <input type="password" value={webdavPassword} onChange={e => setWebdavPassword(e.target.value)} />
          <label>{t('settings.prefix')}</label>
          <input value={webdavPrefix} onChange={e => setWebdavPrefix(e.target.value)} />
          <p className="muted">{t('settings.s3Title')}</p>
          <label>{t('settings.bucket')}</label>
          <input value={s3Bucket} onChange={e => setS3Bucket(e.target.value)} />
          <label>{t('settings.region')}</label>
          <input value={s3Region} onChange={e => setS3Region(e.target.value)} placeholder="us-east-1" />
          <label>{t('settings.endpoint')}</label>
          <input value={s3Endpoint} onChange={e => setS3Endpoint(e.target.value)} />
          <label>{t('settings.accessKeyKeep')}</label>
          <input type="password" value={s3Access} onChange={e => setS3Access(e.target.value)} />
          <label>{t('settings.secretKeyKeep')}</label>
          <input type="password" value={s3Secret} onChange={e => setS3Secret(e.target.value)} />
          <label>{t('settings.sessionTokenKeep')}</label>
          <input type="password" value={s3Session} onChange={e => setS3Session(e.target.value)} />
          <label className="muted" style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
            <input type="checkbox" checked={s3PathStyle} onChange={e => setS3PathStyle(e.target.checked)} />
            {t('settings.pathStyle')}
          </label>
          <label>{t('settings.prefix')}</label>
          <input value={s3Prefix} onChange={e => setS3Prefix(e.target.value)} />
          <p className="muted">{t('settings.secretKeepHint')}</p>
          <button className="primary" type="submit" disabled={busy}>
            {busy ? t('settings.saving') : t('settings.save')}
          </button>
        </form>
        {media && (
          <p className="muted">
            {media.webdav.has_password ? t('settings.webdavPasswordStored') : t('settings.webdavPasswordMissing')} ·{' '}
            {media.s3.has_secret_key ? t('settings.s3SecretStored') : t('settings.s3SecretMissing')}
          </p>
        )}
      </div>

      <div className="card" style={{ maxWidth: 760 }}>
        <h2 style={{ marginTop: 0 }}>{t('settings.providersTitle')}</h2>
        <p className="muted">{t('settings.providersHint')}</p>
        <form onSubmit={saveProvider}>
          <label>{t('settings.provider')}</label>
          <select value={providerId} onChange={e => setProviderId(e.target.value)}>
            {providers.map(item => (
              <option key={item.id} value={item.id}>
                {item.label} {item.configured ? '✓' : ''}
              </option>
            ))}
          </select>
          <label>{t('settings.providerKey')}</label>
          <input type="password" value={providerKey} onChange={e => setProviderKey(e.target.value)} />
          <p className="muted">{t('settings.providerKeyHint')}</p>
          <button className="primary" type="submit" disabled={busy}>
            {busy ? t('settings.saving') : t('settings.save')}
          </button>
        </form>
      </div>
    </>
  )
}
