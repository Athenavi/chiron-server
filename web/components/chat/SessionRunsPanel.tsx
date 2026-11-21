'use client'

/**
 * 会话运行面板：检查点 + 子 Agent。
 *
 * 数据来自 `GET /sessions/{id}/checkpoints|subagents`；恢复检查点前二次确认，
 * 子 Agent 仅对 queued/running 提供取消。
 */

import { useCallback, useEffect, useState } from 'react'
import { Button, Chip, Drawer, Spinner, toast } from '@heroui/react'
import { api } from '@/lib/api'
import { useI18n } from '@/lib/i18n'
import {
  ApiError,
  type ChatSession,
  type CheckpointSummary,
  type SessionFileEntry,
  type SubagentSummary,
} from '@/lib/types'
import {
  AlertIcon,
  ChevronLeftIcon,
  DownloadIcon,
  RefreshIcon,
  StopIcon,
} from '@/components/ui-bridge/icons'

const ACTIVE_SUBAGENT = new Set(['queued', 'running'])

function formatSize(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—'
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`
}

function formatTime(value: string, locale: string): string {
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return value
  return date.toLocaleString(locale === 'en' ? 'en-US' : 'zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  })
}

export interface SessionRunsPanelProps {
  session: ChatSession | null
  isOpen: boolean
  onOpenChange: (open: boolean) => void
}

export function SessionRunsPanel({ session, isOpen, onOpenChange }: SessionRunsPanelProps) {
  const { t, locale } = useI18n()
  const [checkpoints, setCheckpoints] = useState<CheckpointSummary[]>([])
  const [subagents, setSubagents] = useState<SubagentSummary[]>([])
  const [files, setFiles] = useState<SessionFileEntry[]>([])
  const [filePath, setFilePath] = useState('')
  const [filesTruncated, setFilesTruncated] = useState(false)
  const [filesLoading, setFilesLoading] = useState(false)
  const [filesError, setFilesError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [confirmRestore, setConfirmRestore] = useState<string | null>(null)
  const [busyId, setBusyId] = useState<string | null>(null)

  const load = useCallback(async () => {
    if (!session) return
    setLoading(true)
    setError(null)
    try {
      const [checkpointResult, subagentResult] = await Promise.all([
        api.listSessionCheckpoints(session.id, 50),
        api.listSessionSubagents(session.id, 100),
      ])
      setCheckpoints(checkpointResult.checkpoints ?? [])
      setSubagents(subagentResult.subagents ?? [])
    } catch (cause) {
      setError(ApiError.from(cause, t('chat.runs.loadFailed')).message)
    } finally {
      setLoading(false)
    }
  }, [session, t])

  const loadFiles = useCallback(
    async (path: string) => {
      if (!session) return
      setFilesLoading(true)
      setFilesError(null)
      try {
        const result = await api.listSessionFiles(session.id, path, 200)
        setFiles(result.entries ?? [])
        setFilePath(result.path ?? path)
        setFilesTruncated(Boolean(result.truncated))
      } catch (cause) {
        setFilesError(ApiError.from(cause, t('chat.runs.filesFailed')).message)
      } finally {
        setFilesLoading(false)
      }
    },
    [session, t],
  )

  useEffect(() => {
    if (!isOpen) return
    void load()
    void loadFiles('')
  }, [isOpen, load, loadFiles])

  async function restore(id: string) {
    if (busyId) return
    setBusyId(id)
    try {
      await api.restoreCheckpoint(id)
      toast.success(t('chat.runs.restored'))
      setConfirmRestore(null)
      await load()
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('chat.runs.restoreFailed')).message)
    } finally {
      setBusyId(null)
    }
  }

  async function cancelSubagent(id: string) {
    if (busyId) return
    setBusyId(id)
    try {
      await api.cancelSubagent(id)
      toast.success(t('chat.runs.cancelled'))
      await load()
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('chat.runs.cancelFailed')).message)
    } finally {
      setBusyId(null)
    }
  }

  return (
    <Drawer isOpen={isOpen} onOpenChange={onOpenChange}>
      <Drawer.Backdrop>
        <Drawer.Content placement="right">
          <Drawer.Dialog
            aria-label={t('chat.runs.title')}
            className="flex h-full w-[min(28rem,100vw)] flex-col bg-background p-0"
          >
            <Drawer.Header className="flex items-center justify-between gap-2 border-b border-border px-4 py-3">
              <div className="min-w-0">
                <Drawer.Heading className="text-sm font-semibold">
                  {t('chat.runs.title')}
                </Drawer.Heading>
                <p className="mt-0.5 truncate text-[11px] text-muted">
                  {session?.title?.trim() || session?.id || ''}
                </p>
              </div>
              <Button
                variant="ghost"
                size="sm"
                isIconOnly
                aria-label={t('common.refresh')}
                isPending={loading}
                onPress={() => void load()}
                className="h-9 w-9 shrink-0 rounded-md"
              >
                {loading ? null : <RefreshIcon size={14} />}
              </Button>
            </Drawer.Header>

            <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4">
              {error ? (
                <div className="mb-4 flex items-start gap-2 rounded-lg border border-danger/30 bg-danger/5 px-3 py-2 text-[12px] text-danger">
                  <AlertIcon size={14} className="mt-0.5 shrink-0" />
                  <span className="min-w-0 flex-1">{error}</span>
                  <Button variant="ghost" size="sm" className="h-7 px-2" onPress={() => void load()}>
                    {t('common.retry')}
                  </Button>
                </div>
              ) : null}

              {loading && checkpoints.length === 0 && subagents.length === 0 ? (
                <div className="flex items-center justify-center py-16 text-muted">
                  <Spinner size="sm" />
                </div>
              ) : null}

              <section aria-label={t('chat.runs.checkpoints')}>
                <h3 className="mb-2 text-[12px] font-semibold text-foreground">
                  {t('chat.runs.checkpoints')}
                </h3>
                {checkpoints.length === 0 ? (
                  <p className="rounded-lg border border-dashed border-border px-3 py-6 text-center text-[12px] text-muted">
                    {t('chat.runs.emptyCheckpoints')}
                  </p>
                ) : (
                  <ul className="flex flex-col gap-2">
                    {checkpoints.map((checkpoint) => {
                      const restoring = busyId === checkpoint.id
                      return (
                        <li
                          key={checkpoint.id}
                          className="rounded-lg border border-border bg-surface px-3 py-2.5"
                        >
                          <div className="flex flex-wrap items-center gap-1.5">
                            <span className="truncate text-[13px] font-medium text-foreground">
                              {checkpoint.label || checkpoint.kind}
                            </span>
                            <Chip size="sm" variant="soft" color="default">
                              {checkpoint.kind}
                            </Chip>
                            <Chip size="sm" variant="soft" color="default">
                              {checkpoint.status}
                            </Chip>
                          </div>
                          <p className="mt-1 text-[11px] text-muted tabular">
                            #{checkpoint.seq} · {formatTime(checkpoint.created_at, locale)} ·{' '}
                            {(checkpoint.size_bytes / 1024).toFixed(1)} KiB
                          </p>
                          {checkpoint.workspace_commit ? (
                            <p className="mt-1 truncate font-mono text-[10.5px] text-subtle">
                              {t('chat.runs.workspace')}: {checkpoint.workspace_commit.slice(0, 12)}
                            </p>
                          ) : (
                            <p className="mt-1 text-[11px] text-subtle">{t('chat.runs.noWorkspace')}</p>
                          )}

                          {confirmRestore === checkpoint.id ? (
                            <div className="mt-2 rounded-md border border-warning/40 bg-warning/10 px-2.5 py-2">
                              <p className="text-[11.5px] leading-5 text-foreground">
                                {t('chat.runs.restoreConfirm')}
                              </p>
                              <div className="mt-1.5 flex justify-end gap-1.5">
                                <Button
                                  variant="ghost"
                                  size="sm"
                                  className="h-8 px-2 text-[11px]"
                                  onPress={() => setConfirmRestore(null)}
                                >
                                  {t('common.cancel')}
                                </Button>
                                <Button
                                  variant="danger"
                                  size="sm"
                                  isPending={restoring}
                                  className="h-8 px-2 text-[11px]"
                                  onPress={() => void restore(checkpoint.id)}
                                >
                                  {t('chat.runs.confirmRestore')}
                                </Button>
                              </div>
                            </div>
                          ) : checkpoint.workspace_commit ? (
                            <div className="mt-2 flex justify-end">
                              <Button
                                variant="secondary"
                                size="sm"
                                isDisabled={Boolean(busyId)}
                                className="h-8 px-2.5 text-[11px]"
                                onPress={() => setConfirmRestore(checkpoint.id)}
                              >
                                {t('chat.runs.restore')}
                              </Button>
                            </div>
                          ) : null}
                        </li>
                      )
                    })}
                  </ul>
                )}
              </section>

              <section aria-label={t('chat.runs.subagents')} className="mt-5">
                <h3 className="mb-2 text-[12px] font-semibold text-foreground">
                  {t('chat.runs.subagents')}
                </h3>
                {subagents.length === 0 ? (
                  <p className="rounded-lg border border-dashed border-border px-3 py-6 text-center text-[12px] text-muted">
                    {t('chat.runs.emptySubagents')}
                  </p>
                ) : (
                  <ul className="flex flex-col gap-2">
                    {subagents.map((subagent) => {
                      const active = ACTIVE_SUBAGENT.has(subagent.status)
                      return (
                        <li
                          key={subagent.id}
                          className="rounded-lg border border-border bg-surface px-3 py-2.5"
                        >
                          <div className="flex flex-wrap items-center gap-1.5">
                            <span className="truncate text-[13px] font-medium text-foreground">
                              {subagent.agent}
                            </span>
                            <Chip
                              size="sm"
                              variant="soft"
                              color={active ? 'warning' : 'default'}
                            >
                              {subagent.status}
                            </Chip>
                            <span className="text-[11px] text-muted tabular">
                              {t('chat.runs.depth', { n: subagent.depth })}
                            </span>
                          </div>
                          <p className="mt-1 line-clamp-3 text-[11.5px] leading-5 text-muted">
                            {subagent.task}
                          </p>
                          <p className="mt-1 text-[11px] text-subtle tabular">
                            {formatTime(subagent.created_at, locale)}
                            {subagent.model ? ` · ${subagent.model}` : ''}
                          </p>
                          {active ? (
                            <div className="mt-2 flex justify-end">
                              <Button
                                variant="danger-soft"
                                size="sm"
                                className="h-8 gap-1 px-2.5 text-[11px]"
                                isPending={busyId === subagent.id}
                                isDisabled={Boolean(busyId) && busyId !== subagent.id}
                                onPress={() => void cancelSubagent(subagent.id)}
                              >
                                <StopIcon size={12} />
                                {t('chat.runs.cancelSubagent')}
                              </Button>
                            </div>
                          ) : null}
                        </li>
                      )
                    })}
                  </ul>
                )}
              </section>

              <section aria-label={t('chat.runs.files')} className="mt-5">
                <div className="mb-2 flex items-center justify-between gap-2">
                  <h3 className="text-[12px] font-semibold text-foreground">
                    {t('chat.runs.files')}
                  </h3>
                  <div className="flex items-center gap-1">
                    {filePath ? (
                      <Button
                        variant="ghost"
                        size="sm"
                        isIconOnly
                        aria-label={t('chat.runs.up')}
                        onPress={() =>
                          void loadFiles(filePath.split('/').slice(0, -1).join('/'))
                        }
                        className="h-8 w-8 rounded-md"
                      >
                        <ChevronLeftIcon size={13} />
                      </Button>
                    ) : null}
                    <Button
                      variant="ghost"
                      size="sm"
                      isIconOnly
                      aria-label={t('common.refresh')}
                      isPending={filesLoading}
                      onPress={() => void loadFiles(filePath)}
                      className="h-8 w-8 rounded-md"
                    >
                      {filesLoading ? null : <RefreshIcon size={13} />}
                    </Button>
                  </div>
                </div>
                <p className="mb-2 truncate font-mono text-[10.5px] text-subtle" title={filePath}>
                  /{filePath}
                </p>

                {filesError ? (
                  <p className="rounded-lg border border-danger/30 bg-danger/5 px-3 py-2 text-[11.5px] text-danger">
                    {filesError}
                  </p>
                ) : files.length === 0 && !filesLoading ? (
                  <p className="rounded-lg border border-dashed border-border px-3 py-6 text-center text-[12px] text-muted">
                    {t('chat.runs.emptyFiles')}
                  </p>
                ) : (
                  <ul className="overflow-hidden rounded-lg border border-border bg-surface">
                    {files.map((entry) => {
                      const directory = entry.kind === 'dir'
                      const file = entry.kind === 'file'
                      return (
                        <li
                          key={entry.path}
                          className="flex items-center gap-2 border-b border-border px-3 py-2 last:border-b-0"
                        >
                          <span className="min-w-0 flex-1">
                            {directory ? (
                              <button
                                type="button"
                                onClick={() => void loadFiles(entry.path)}
                                className="block max-w-full truncate text-left text-[12.5px] text-foreground hover:text-accent"
                              >
                                {entry.name}/
                              </button>
                            ) : (
                              <span className="block truncate text-[12.5px] text-foreground">
                                {entry.name}
                              </span>
                            )}
                            <span className="mt-0.5 block text-[10.5px] text-subtle tabular">
                              {t(`chat.runs.kind.${entry.kind}`)}
                              {file ? ` · ${formatSize(entry.size)}` : ''}
                              {entry.modified_at
                                ? ` · ${formatTime(entry.modified_at, locale)}`
                                : ''}
                            </span>
                          </span>
                          {file ? (
                            <a
                              href={api.sessionFileDownloadUrl(session?.id ?? '', entry.path)}
                              download={entry.name}
                              aria-label={t('chat.runs.downloadFile', { name: entry.name })}
                              className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-muted hover:bg-surface-hover hover:text-foreground"
                            >
                              <DownloadIcon size={14} />
                            </a>
                          ) : null}
                        </li>
                      )
                    })}
                  </ul>
                )}
                {filesTruncated ? (
                  <p className="mt-1.5 text-[10.5px] text-muted">{t('chat.runs.truncated')}</p>
                ) : null}
              </section>
            </div>
          </Drawer.Dialog>
        </Drawer.Content>
      </Drawer.Backdrop>
    </Drawer>
  )
}

export default SessionRunsPanel
