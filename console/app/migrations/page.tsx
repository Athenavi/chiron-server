'use client'

import { useEffect, useState } from 'react'
import { useRouter } from 'next/navigation'
import { api, type MigrationStatus, type UpgradeResult } from '@/lib/api'
import { useI18n } from '@/lib/i18n'

export default function MigrationsPage() {
  const router = useRouter()
  const { t } = useI18n()
  const [status, setStatus] = useState<MigrationStatus | null>(null)
  const [result, setResult] = useState<UpgradeResult | null>(null)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)

  async function load() {
    try {
      setStatus(await api.migrations())
    } catch (err) {
      if ((err as { status?: number })?.status === 401) router.replace('/login')
      else setError(err instanceof Error ? err.message : t('common.loadFailed'))
    }
  }

  useEffect(() => {
    void load()
  }, [])

  async function run(dryRun: boolean) {
    if (!dryRun && !confirm(t('migrations.confirm'))) return
    setBusy(true)
    setError('')
    try {
      const res = await api.upgradeMigrations(dryRun)
      setResult(res)
      await load()
    } catch (err) {
      setError(err instanceof Error ? err.message : t('migrations.runFailed'))
    } finally {
      setBusy(false)
    }
  }

  const target = status?.target_version ?? 0
  const pending = status?.tenants.filter(tv => tv.schema_version < target).length ?? 0

  return (
    <>
      <h1 style={{ marginTop: 0 }}>{t('migrations.title')}</h1>
      <p className="muted">{t('migrations.summary', { target, pending })}</p>
      <div className="row" style={{ marginBottom: 18 }}>
        <button disabled={busy} onClick={() => run(true)}>
          {t('migrations.dryRun')}
        </button>
        <button className="primary" disabled={busy || pending === 0} onClick={() => run(false)}>
          {t('migrations.upgradeAll')}
        </button>
      </div>
      {error && <p className="error">{error}</p>}
      {result && (
        <div className="card" style={{ marginBottom: 18 }}>
          <strong>
            {result.dry_run ? t('migrations.dryRunResult') : t('migrations.upgradeResult')} · {result.status}
            {t('migrations.resultCounts', {
              ok: result.ok_count,
              failed: result.failed_count,
              skipped: result.skipped_count,
            })}
          </strong>
          <table>
            <thead>
              <tr>
                <th>{t('migrations.colTenant')}</th>
                <th>{t('migrations.colVersion')}</th>
                <th>{t('migrations.colResult')}</th>
              </tr>
            </thead>
            <tbody>
              {result.detail.map(d => (
                <tr key={d.id}>
                  <td>{d.slug}</td>
                  <td>
                    {d.from} → {d.to}
                  </td>
                  <td className={d.status === 'failed' ? 'error' : 'muted'}>
                    {d.status}
                    {d.pending ? t('migrations.pendingMigrations') : ''}
                    {d.error ? `: ${d.error}` : ''}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <div className="card">
        <h2 style={{ marginTop: 0 }}>{t('migrations.versionsTitle')}</h2>
        <table>
          <thead>
            <tr>
              <th>Slug</th>
              <th>{t('migrations.colStatus')}</th>
              <th>schema</th>
              <th>{t('migrations.colLastMigration')}</th>
            </tr>
          </thead>
          <tbody>
            {(status?.tenants ?? []).map(tv => (
              <tr key={tv.id}>
                <td>{tv.slug}</td>
                <td className="muted">{tv.status}</td>
                <td>
                  {tv.schema_version}
                  {tv.schema_version < target ? <span className="error">{t('migrations.behind')}</span> : null}
                </td>
                <td className="muted">{tv.migrated_at ?? '—'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {status && status.runs.length > 0 && (
        <div className="card" style={{ marginTop: 18 }}>
          <h2 style={{ marginTop: 0 }}>{t('migrations.runsTitle')}</h2>
          <table>
            <thead>
              <tr>
                <th>{t('migrations.colTime')}</th>
                <th>{t('migrations.colActor')}</th>
                <th>{t('migrations.colType')}</th>
                <th>{t('migrations.colResult')}</th>
              </tr>
            </thead>
            <tbody>
              {status.runs.map(r => (
                <tr key={r.id}>
                  <td className="muted">{r.started_at}</td>
                  <td>{r.actor_email}</td>
                  <td>{r.dry_run ? t('migrations.runKindCheck') : t('migrations.runKindUpgrade')}</td>
                  <td className="muted">
                    {r.status} · ok {r.ok_count} / failed {r.failed_count} / skipped {r.skipped_count}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </>
  )
}
