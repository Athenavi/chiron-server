'use client'

import { useEffect, useState } from 'react'
import { useRouter } from 'next/navigation'
import { api, type Plan } from '@/lib/api'
import { useI18n } from '@/lib/i18n'

export default function PlansPage() {
  const router = useRouter()
  const { t } = useI18n()
  const [plans, setPlans] = useState<Plan[]>([])
  const [error, setError] = useState('')

  useEffect(() => {
    api.plans().then(setPlans).catch(err => {
      if (err?.status === 401) router.replace('/login')
      else setError(err instanceof Error ? err.message : t('common.loadFailed'))
    })
  }, [router, t])

  return (
    <>
      <h1 style={{ marginTop: 0 }}>{t('plans.title')}</h1>
      <p className="muted">{t('plans.subtitle')}</p>
      {error && <p className="error">{error}</p>}
      <div className="grid">
        {plans.map(p => (
          <div className="card" key={p.id}>
            <h2 style={{ marginTop: 0 }}>{p.name}</h2>
            <p className="muted">
              {p.id} · {p.isolation_tier} · egress {p.egress_mode ?? 'bridge'}
            </p>
            <table>
              <tbody>
                <tr><th>{t('plans.users')}</th><td>{p.max_users}</td></tr>
                <tr><th>{t('plans.concurrentRuns')}</th><td>{p.max_concurrent_runs}</td></tr>
                <tr><th>{t('plans.monthlyTokens')}</th><td>{p.monthly_tokens}</td></tr>
                <tr><th>{t('plans.storageGb')}</th><td>{p.max_storage_gb}</td></tr>
                <tr><th>{t('plans.sandboxes')}</th><td>{p.max_sandboxes}</td></tr>
                <tr>
                  <th>{t('plans.monthlyCostCap')}</th>
                  <td>{p.monthly_cost_cents ? t('plans.costCents', { cents: p.monthly_cost_cents }) : t('plans.unlimited')}</td>
                </tr>
                <tr>
                  <th>{t('plans.overage')}</th>
                  <td>{p.overage_policy === 'allow' ? t('plans.overageAllow') : t('plans.overageBlock')}</td>
                </tr>
                <tr>
                  <th>{t('plans.modelAllowlist')}</th>
                  <td>{(p.model_allowlist ?? []).length ? (p.model_allowlist ?? []).join(', ') : t('plans.unlimited')}</td>
                </tr>
                <tr>
                  <th>{t('plans.egressAllowlist')}</th>
                  <td>{t('plans.egressCount', { count: (p.egress_allowlist ?? []).length })}</td>
                </tr>
              </tbody>
            </table>
          </div>
        ))}
      </div>
    </>
  )
}
