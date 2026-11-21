'use client'

import { useEffect, useState } from 'react'
import { useParams, useRouter } from 'next/navigation'
import { api, downloadTenantExport, type BillReconcile, type Domain, type Invoice, type ProviderKey, type Tenant } from '@/lib/api'
import { useI18n } from '@/lib/i18n'

export default function TenantDetailPage() {
  const router = useRouter()
  const { t } = useI18n()
  const params = useParams<{ id: string }>()
  const id = params.id
  const [tenant, setTenant] = useState<Tenant | null>(null)
  const [domains, setDomains] = useState<Domain[]>([])
  const [domainHost, setDomainHost] = useState('')
  const [providerKeys, setProviderKeys] = useState<ProviderKey[]>([])
  const [keyProvider, setKeyProvider] = useState('')
  const [keyValue, setKeyValue] = useState('')
  const [credentials, setCredentials] = useState<{ email: string; password: string } | null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const [deleteBackup, setDeleteBackup] = useState(true)
  const [billingMonth, setBillingMonth] = useState(() => new Date().toISOString().slice(0, 7))
  const [invoice, setInvoice] = useState<Invoice | null>(null)
  const [reconcile, setReconcile] = useState<BillReconcile | null>(null)

  async function load() {
    try {
      const [tenantRes, domainRes, keyRes] = await Promise.all([
        api.tenant(id),
        api.domains(id),
        api.providerKeys(id),
      ])
      setTenant(tenantRes)
      setDomains(domainRes)
      setProviderKeys(keyRes)
    } catch (err) {
      if ((err as { status?: number })?.status === 401) router.replace('/login')
      else setError(err instanceof Error ? err.message : t('common.loadFailed'))
    }
  }

  useEffect(() => {
    if (id) void load()
  }, [id])

  async function toggle() {
    if (!tenant) return
    const resuming = tenant.status === 'suspended'
    if (!window.confirm(t(resuming ? 'tenant.resumeConfirm' : 'tenant.suspendConfirm'))) return
    setBusy(true)
    setError('')
    try {
      const next = tenant.status === 'suspended' ? await api.resume(id) : await api.suspend(id)
      setTenant(next)
    } catch (err) {
      setError(err instanceof Error ? err.message : t('common.operationFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function provisionTenant() {
    setBusy(true)
    setError('')
    try {
      setTenant(await api.provision(id))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.provisionFailed'))
      await load()
    } finally {
      setBusy(false)
    }
  }

  async function revealCredentials() {
    setBusy(true)
    setError('')
    try {
      setCredentials(await api.ownerCredentials(id))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.credentialsFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function addDomain(e: React.FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError('')
    try {
      await api.addDomain(id, domainHost.trim())
      setDomainHost('')
      setDomains(await api.domains(id))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.domainsAddFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function verifyDomain(domainId: string) {
    setBusy(true)
    setError('')
    try {
      await api.verifyDomain(id, domainId)
      setDomains(await api.domains(id))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.domainsVerifyFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function probeDomainTls(domainId: string) {
    setBusy(true)
    setError('')
    try {
      const result = await api.probeDomainTls(id, domainId)
      setDomains(await api.domains(id))
      if (result.error) setError(t('tenant.domainsTlsFailed', { error: result.error }))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.domainsTlsError'))
    } finally {
      setBusy(false)
    }
  }

  async function saveProviderKey(e: React.FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError('')
    try {
      await api.putProviderKey(id, keyProvider.trim(), keyValue.trim())
      setKeyProvider('')
      setKeyValue('')
      setProviderKeys(await api.providerKeys(id))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('common.saveFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function exportTenant() {
    if (!tenant) return
    setBusy(true)
    setError('')
    try {
      await downloadTenantExport(id, `chiron-${tenant.slug}-export.json`)
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.exportFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function backupTenant() {
    setBusy(true)
    setError('')
    try {
      const res = await api.backupTenant(id)
      window.alert(t('tenant.backupDone', { file: res.file, bytes: res.bytes }))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.backupFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function deleteTenant() {
    if (!tenant) return
    const confirm = window.prompt(t('tenant.deleteConfirm', { slug: tenant.slug }))
    if (confirm === null) return
    setBusy(true)
    setError('')
    try {
      await api.deleteTenant(id, confirm, deleteBackup)
      router.replace('/tenants')
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.deleteFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function loadInvoice() {
    setBusy(true)
    setError('')
    try {
      setInvoice(await api.invoice(id, billingMonth))
      setReconcile(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.billingFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function loadReconcile() {
    setBusy(true)
    setError('')
    try {
      setReconcile(await api.reconcileBill(id, billingMonth))
      setInvoice(null)
    } catch (err) {
      setError(err instanceof Error ? err.message : t('tenant.billingFailed'))
    } finally {
      setBusy(false)
    }
  }

  async function removeProviderKey(provider: string) {
    if (!window.confirm(t('tenant.providerKeyDeleteConfirm', { provider }))) return
    setBusy(true)
    setError('')
    try {
      await api.deleteProviderKey(id, provider)
      setProviderKeys(await api.providerKeys(id))
    } catch (err) {
      setError(err instanceof Error ? err.message : t('common.deleteFailed'))
    } finally {
      setBusy(false)
    }
  }

  if (!tenant) {
    return <p className="muted">{error || t('common.loading')}</p>
  }

  return (
    <>
      <h1 style={{ marginTop: 0 }}>{tenant.name}</h1>
      <p className="muted">
        {tenant.slug} · {tenant.plan_id} · {tenant.db_name}
      </p>
      <div className="row" style={{ marginBottom: 18 }}>
        <span className="badge">{tenant.status}</span>
        <button className={tenant.status === 'suspended' ? 'primary' : 'danger'} disabled={busy} onClick={toggle}>
          {tenant.status === 'suspended' ? t('tenant.resume') : t('tenant.suspend')}
        </button>
        {(tenant.status === 'pending' || tenant.status === 'failed') && (
          <button className="primary" disabled={busy} onClick={provisionTenant}>
            {tenant.status === 'failed' ? t('tenant.provisionRetry') : t('tenant.provision')}
          </button>
        )}
        {tenant.status === 'active' && (
          <button disabled={busy} onClick={revealCredentials}>
            {t('tenant.credentialsButton')}
          </button>
        )}
        {(tenant.status === 'active' || tenant.status === 'suspended') && (
          <button disabled={busy} onClick={exportTenant}>
            {t('tenant.export')}
          </button>
        )}
        {(tenant.status === 'active' || tenant.status === 'suspended') && (
          <button disabled={busy} onClick={backupTenant}>
            {t('tenant.backup')}
          </button>
        )}
        {tenant.status === 'suspended' && (
          <>
            <label className="muted" style={{ display: 'inline-flex', alignItems: 'center', gap: 6 }}>
              <input
                type="checkbox"
                checked={deleteBackup}
                disabled={busy}
                onChange={(event) => setDeleteBackup(event.target.checked)}
              />
              {t('tenant.deleteBackup')}
            </label>
            <button className="danger" disabled={busy} onClick={deleteTenant}>
              {t('tenant.delete')}
            </button>
          </>
        )}
      </div>
      {tenant.provision_error && (
        <div className="card" style={{ marginBottom: 18 }}>
          <strong>{t('tenant.provisionFailed')}:</strong>
          <p className="error" style={{ marginBottom: 0 }}>{tenant.provision_error}</p>
        </div>
      )}
      {credentials && (
        <div className="card" style={{ marginBottom: 18 }}>
          <strong>{t('tenant.credentialsTitle')}</strong>
          <p style={{ marginBottom: 4 }}>{credentials.email}</p>
          <code>{credentials.password}</code>
          <p className="muted">{t('tenant.credentialsHint')}</p>
        </div>
      )}

      <div className="card" style={{ marginBottom: 18 }}>
        <h2 style={{ marginTop: 0 }}>{t('tenant.domainsTitle')}</h2>
        <table>
          <thead>
            <tr>
              <th>Host</th>
              <th>{t('tenant.domainsColKind')}</th>
              <th>{t('tenant.domainsColVerify')}</th>
              <th>TLS</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {domains.map(d => (
              <tr key={d.id}>
                <td>{d.host}</td>
                <td>{d.kind}</td>
                <td className="muted">{d.verified_at ? t('tenant.domainsVerified') : t('tenant.domainsPending')}</td>
                <td className="muted">{d.tls_status}</td>
                <td>
                  {d.kind === 'custom' && !d.verified_at && (
                    <button disabled={busy} onClick={() => verifyDomain(d.id)}>
                      {t('tenant.domainsVerifyTxt')}
                    </button>
                  )}{' '}
                  <button
                    type="button"
                    className="muted"
                    disabled={busy || (d.kind === 'custom' && !d.verified_at)}
                    onClick={() => void probeDomainTls(d.id)}
                  >
                    {t('tenant.domainsProbeTls')}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <form onSubmit={addDomain} className="row" style={{ marginTop: 14 }}>
          <input
            value={domainHost}
            onChange={e => setDomainHost(e.target.value)}
            placeholder="chat.customer.com"
            style={{ flex: 1, minWidth: 240 }}
          />
          <button disabled={busy} type="submit">
            {t('tenant.domainsBind')}
          </button>
        </form>
        <p className="muted">{t('tenant.domainsHint')}</p>
      </div>

      <div className="card" style={{ marginBottom: 18 }}>
        <h2 style={{ marginTop: 0 }}>{t('tenant.billingTitle')}</h2>
        <p className="muted">{t('tenant.billingHint')}</p>
        <div className="row">
          <input
            value={billingMonth}
            onChange={(event) => setBillingMonth(event.target.value)}
            placeholder="YYYY-MM"
          />
          <button disabled={busy} onClick={loadInvoice}>
            {t('tenant.invoice')}
          </button>
          <button disabled={busy} onClick={loadReconcile}>
            {t('tenant.reconcile')}
          </button>
        </div>
        {invoice && <pre className="muted">{JSON.stringify(invoice, null, 2)}</pre>}
        {reconcile && (
          <pre className={reconcile.drift ? 'error' : 'muted'}>
            {JSON.stringify(reconcile, null, 2)}
          </pre>
        )}
      </div>

      <div className="card" style={{ marginBottom: 18 }}>
        <h2 style={{ marginTop: 0 }}>{t('tenant.providerKeysTitle')}</h2>
        <p className="muted">{t('tenant.providerKeysHint')}</p>
        <table>
          <thead>
            <tr>
              <th>Provider</th>
              <th>{t('tenant.providerKeysColUpdated')}</th>
              <th>{t('tenant.providerKeysColActions')}</th>
            </tr>
          </thead>
          <tbody>
            {providerKeys.map(k => (
              <tr key={k.provider}>
                <td>{k.provider}</td>
                <td className="muted">{k.updated_at}</td>
                <td>
                  <button
                    className="danger"
                    disabled={busy}
                    onClick={() => removeProviderKey(k.provider)}
                  >
                    {t('common.delete')}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <form onSubmit={saveProviderKey} className="row" style={{ marginTop: 14 }}>
          <input
            value={keyProvider}
            onChange={e => setKeyProvider(e.target.value)}
            placeholder="openai / anthropic / deepseek …"
            style={{ width: 220 }}
          />
          <input
            value={keyValue}
            onChange={e => setKeyValue(e.target.value)}
            placeholder={t('tenant.providerKeysValuePlaceholder')}
            type="password"
            autoComplete="off"
            style={{ flex: 1, minWidth: 240 }}
          />
          <button disabled={busy} type="submit">
            {t('tenant.providerKeysSave')}
          </button>
        </form>
      </div>
      {error && <p className="error">{error}</p>}
    </>
  )
}
