'use client'

import { useEffect, useState } from 'react'
import { useRouter } from 'next/navigation'
import { api, type Plan } from '@/lib/api'
import { useI18n } from '@/lib/i18n'

export default function NewTenantPage() {
  const router = useRouter()
  const { t } = useI18n()
  const [plans, setPlans] = useState<Plan[]>([])
  const [slug, setSlug] = useState('')
  const [name, setName] = useState('')
  const [planId, setPlanId] = useState('standard')
  const [region, setRegion] = useState('default')
  const [ownerEmail, setOwnerEmail] = useState('')
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  useEffect(() => {
    api.plans().then(setPlans).catch(err => {
      if (err?.status === 401) router.replace('/login')
      else setError(err instanceof Error ? err.message : t('tenantNew.plansFailed'))
    })
  }, [router, t])

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError('')
    try {
      const tenant = await api.createTenant({
        slug: slug.trim().toLowerCase(),
        name: name.trim(),
        plan_id: planId,
        region: region.trim() || 'default',
        owner_email: ownerEmail.trim().toLowerCase(),
      })
      router.replace(`/tenants/${tenant.id}`)
    } catch (err) {
      setError(err instanceof Error ? err.message : t('common.createFailed'))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="card" style={{ maxWidth: 640 }}>
      <h1 style={{ marginTop: 0 }}>{t('tenantNew.title')}</h1>
      <p className="muted">{t('tenantNew.subtitle')}</p>
      <form onSubmit={submit}>
        <label>{t('tenantNew.slugLabel')}</label>
        <input value={slug} onChange={e => setSlug(e.target.value)} placeholder="acme" />
        <label>{t('tenantNew.nameLabel')}</label>
        <input value={name} onChange={e => setName(e.target.value)} placeholder="Acme Inc." />
        <label>{t('tenantNew.planLabel')}</label>
        <select value={planId} onChange={e => setPlanId(e.target.value)}>
          {plans.map(p => (
            <option key={p.id} value={p.id}>
              {p.name} · {p.isolation_tier}
            </option>
          ))}
        </select>
        <label>{t('tenantNew.regionLabel')}</label>
        <input value={region} onChange={e => setRegion(e.target.value)} placeholder="default" />
        <label>{t('tenantNew.ownerLabel')}</label>
        <input
          value={ownerEmail}
          required
          onChange={e => setOwnerEmail(e.target.value)}
          placeholder="owner@acme.com"
        />
        <div style={{ marginTop: 18 }}>
          <button className="primary" disabled={busy} type="submit">
            {busy ? t('tenantNew.submitting') : t('tenantNew.submit')}
          </button>
        </div>
      </form>
      {error && <p className="error">{error}</p>}
    </div>
  )
}
