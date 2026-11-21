'use client'

import { useEffect, useState } from 'react'
import Link from 'next/link'
import { useRouter } from 'next/navigation'
import { api, type Tenant } from '@/lib/api'
import { useI18n } from '@/lib/i18n'

export default function TenantsPage() {
  const router = useRouter()
  const { t } = useI18n()
  const [tenants, setTenants] = useState<Tenant[]>([])
  const [error, setError] = useState('')
  const [loading, setLoading] = useState(true)

  useEffect(() => {
    api
      .tenants()
      .then(setTenants)
      .catch(err => {
        if (err?.status === 401) router.replace('/login')
        else setError(err instanceof Error ? err.message : t('common.loadFailed'))
      })
      .finally(() => setLoading(false))
  }, [router, t])

  return (
    <>
      <div className="row" style={{ justifyContent: 'space-between', marginBottom: 18 }}>
        <div>
          <h1 style={{ margin: 0 }}>{t('tenants.title')}</h1>
          <p className="muted">{t('tenants.subtitle')}</p>
        </div>
        <Link className="badge" href="/tenants/new">
          {t('tenants.create')}
        </Link>
      </div>
      {loading && <p className="muted">{t('common.loading')}</p>}
      {error && <p className="error">{error}</p>}
      <div className="card">
        <table>
          <thead>
            <tr>
              <th>Slug</th>
              <th>{t('tenants.colName')}</th>
              <th>{t('tenants.colPlan')}</th>
              <th>{t('tenants.colStatus')}</th>
              <th>{t('tenants.colRegion')}</th>
              <th>{t('tenants.colDatabase')}</th>
            </tr>
          </thead>
          <tbody>
            {tenants.map(tn => (
              <tr key={tn.id}>
                <td>
                  <Link href={`/tenants/${tn.id}`}>{tn.slug}</Link>
                </td>
                <td>{tn.name}</td>
                <td>{tn.plan_id}</td>
                <td>
                  <span className="badge">{tn.status}</span>
                </td>
                <td>{tn.region}</td>
                <td className="muted">{tn.db_name}</td>
              </tr>
            ))}
            {!loading && tenants.length === 0 && (
              <tr>
                <td colSpan={6} className="muted">
                  {t('tenants.empty')}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </>
  )
}
