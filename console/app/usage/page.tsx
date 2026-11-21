'use client'

import { useEffect, useState } from 'react'
import { useRouter } from 'next/navigation'
import { api, type UsageRollup, type UsageRow } from '@/lib/api'
import { useI18n } from '@/lib/i18n'

function tokens(n: number) {
  return n.toLocaleString()
}

export default function UsagePage() {
  const router = useRouter()
  const { t } = useI18n()
  const [rows, setRows] = useState<UsageRow[]>([])
  const [days, setDays] = useState(30)
  const [result, setResult] = useState<UsageRollup | null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  async function load() {
    try {
      setRows(await api.usage())
    } catch (err) {
      if ((err as { status?: number })?.status === 401) router.replace('/login')
      else setError(err instanceof Error ? err.message : t('common.loadFailed'))
    }
  }

  useEffect(() => {
    void load()
  }, [])

  async function rollup() {
    setBusy(true)
    setError('')
    try {
      const res = await api.rollupUsage(days)
      setResult(res)
      await load()
    } catch (err) {
      setError(err instanceof Error ? err.message : t('usage.rollupFailed'))
    } finally {
      setBusy(false)
    }
  }

  const rollupErrors = (result?.detail ?? [])
    .filter(d => d.error)
    .map(d => ` ${d.slug}: ${d.error}`)
    .join('; ')

  return (
    <>
      <h1 style={{ marginTop: 0 }}>{t('usage.title')}</h1>
      <p className="muted">{t('usage.subtitle')}</p>
      <div className="row" style={{ marginBottom: 18 }}>
        <label style={{ margin: 0 }}>{t('usage.windowDays')}</label>
        <input
          type="number"
          min={1}
          max={90}
          value={days}
          onChange={e => setDays(Number(e.target.value) || 30)}
          style={{ width: 90 }}
        />
        <button className="primary" disabled={busy} onClick={rollup}>
          {busy ? t('usage.rolling') : t('usage.rollup')}
        </button>
      </div>
      {error && <p className="error">{error}</p>}
      {result && (
        <p className="muted">
          {t('usage.rollupSummary', {
            days: result.days,
            ok: result.tenants_ok,
            failed: result.tenants_failed,
            rows: result.rows,
          })}
          {rollupErrors ? ` ${rollupErrors}` : ''}
        </p>
      )}
      <div className="card">
        <table>
          <thead>
            <tr>
              <th>{t('usage.colTenant')}</th>
              <th>{t('usage.colDay')}</th>
              <th>runs</th>
              <th>{t('usage.colTokensIn')}</th>
              <th>{t('usage.colTokensOut')}</th>
              <th>{t('usage.colCost')}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map(r => (
              <tr key={`${r.tenant_id}-${r.day}`}>
                <td>{r.slug ?? r.tenant_id}</td>
                <td className="muted">{r.day}</td>
                <td>{r.runs}</td>
                <td>{tokens(r.tokens_in)}</td>
                <td>{tokens(r.tokens_out)}</td>
                <td>{r.cost_cents}</td>
              </tr>
            ))}
          </tbody>
        </table>
        {rows.length === 0 && <p className="muted">{t('usage.empty')}</p>}
      </div>
    </>
  )
}
