'use client'

/**
 * 文件 / 媒体页（网盘式）。
 *
 * - 上传：页头按钮与拖拽区共用同一个隐藏 `<input type="file" multiple>`，
 *   逐个上传并显示 x/y 进度；单个失败只 toast，不阻塞后续文件，全部结束后刷新列表；
 * - 列表：本地按文件名 / MIME 过滤；图片显示懒加载缩略图（失败回落文件图标），
 *   非图片按扩展名 / MIME 显示类型图标；操作有预览（仅图片）、下载、删除（确认弹窗）；
 * - 分页：`next_cursor` 存在时「加载更多」并按 id 去重；骨架 / 空态 / 错误重试齐备。
 */

import { useCallback, useEffect, useMemo, useRef, useState, type ChangeEvent } from 'react'
import { Alert, Button, EmptyState, Input, Label, TextField, toast } from '@heroui/react'
import { PageHeader } from '@/components/shell/PageHeader'
import { AlertIcon, RefreshIcon, SearchIcon, XIcon } from '@/components/ui-bridge/icons'
import { DeleteMediaDialog } from '@/components/media/DeleteMediaDialog'
import { MediaCard } from '@/components/media/MediaCard'
import { MediaGridSkeleton } from '@/components/media/MediaGridSkeleton'
import { MediaPreviewModal } from '@/components/media/MediaPreviewModal'
import { MediaUploadZone } from '@/components/media/MediaUploadZone'
import { UploadIcon } from '@/components/media/MediaIcons'
import { api } from '@/lib/api'
import { useI18n } from '@/lib/i18n'
import { ApiError, type MediaAsset } from '@/lib/types'

/** 与后端默认分页上限一致。 */
const PAGE_SIZE = 24
/** 后端单文件上限（50 MiB），入队前先过滤，避免白跑一次请求。 */
const MAX_UPLOAD_BYTES = 50 * 1024 * 1024
const UPLOAD_CONCURRENCY = 3

/** 网格列宽：移动端单列，桌面逐级加宽（与骨架保持一致）。 */
const GRID_CLASS = 'grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4'

interface UploadProgress {
  total: number
  done: number
  succeeded: number
  failed: number
}

interface FailedUpload {
  id: string
  file: File
  message: string
}

function uploadId(file: File): string {
  return `${file.name}:${file.size}:${file.lastModified}:${Math.random().toString(36).slice(2, 8)}`
}

/** 追加时按 id 去重，保持接口返回的时间倒序。 */
function mergeAssets(current: MediaAsset[], incoming: MediaAsset[]): MediaAsset[] {
  const seen = new Set(current.map((item) => item.id))
  const merged = [...current]
  for (const asset of incoming) {
    if (seen.has(asset.id)) continue
    seen.add(asset.id)
    merged.push(asset)
  }
  return merged
}

export default function MediaPage() {
  const { t } = useI18n()

  const [assets, setAssets] = useState<MediaAsset[]>([])
  const [nextCursor, setNextCursor] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [loadingMore, setLoadingMore] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const [query, setQuery] = useState('')
  const [upload, setUpload] = useState<UploadProgress | null>(null)
  const [failures, setFailures] = useState<FailedUpload[]>([])
  const [retryingId, setRetryingId] = useState<string | null>(null)

  const [preview, setPreview] = useState<MediaAsset | null>(null)
  const [deleteTarget, setDeleteTarget] = useState<MediaAsset | null>(null)
  const [deleting, setDeleting] = useState(false)
  const [deleteError, setDeleteError] = useState<string | null>(null)

  const fileInputRef = useRef<HTMLInputElement | null>(null)
  /** 刷新 / 加载更多各自的请求序号：慢响应回来时丢弃，避免互相覆盖。 */
  const loadSeq = useRef(0)
  const moreSeq = useRef(0)
  /** 上传批次的同步互斥锁（state 更新是异步的，不能只靠 isUploading 判断）。 */
  const uploadingRef = useRef(false)
  const abortRef = useRef<AbortController | null>(null)

  const isUploading = upload !== null

  /** 重新拉第一页；返回是否成功，供上传结束后的刷新提示使用。 */
  const load = useCallback(async (): Promise<boolean> => {
    const seq = (loadSeq.current += 1)
    setLoading(true)
    setError(null)
    try {
      const result = await api.listMedia({ limit: PAGE_SIZE })
      // 期间已有更新的刷新请求：这页作废，但不算失败
      if (seq !== loadSeq.current) return true
      setAssets(mergeAssets([], result.media))
      setNextCursor(result.next_cursor)
      return true
    } catch (cause) {
      if (seq !== loadSeq.current) return true
      setError(ApiError.from(cause, t('media.error.fallback')).message)
      return false
    } finally {
      if (seq === loadSeq.current) setLoading(false)
    }
  }, [t])

  useEffect(() => {
    void load()
  }, [load])

  const loadMore = useCallback(async () => {
    const cursor = nextCursor
    if (!cursor || loadingMore || loading) return
    const seq = (moreSeq.current += 1)
    const loadMark = loadSeq.current
    setLoadingMore(true)
    try {
      const result = await api.listMedia({ cursor, limit: PAGE_SIZE })
      // 期间发生过刷新：丢弃这页，避免旧游标数据混进新列表
      if (seq !== moreSeq.current || loadMark !== loadSeq.current) return
      setAssets((previous) => mergeAssets(previous, result.media))
      setNextCursor(result.next_cursor)
    } catch (cause) {
      if (seq !== moreSeq.current || loadMark !== loadSeq.current) return
      toast.danger(ApiError.from(cause, t('media.error.loadMore')).message)
    } finally {
      if (seq === moreSeq.current) setLoadingMore(false)
    }
  }, [loading, loadingMore, nextCursor, t])

  function openPicker() {
    if (uploadingRef.current) return
    fileInputRef.current?.click()
  }

  async function uploadFiles(files: File[]) {
    if (uploadingRef.current || files.length === 0) return

    const oversized = files.filter((file) => file.size > MAX_UPLOAD_BYTES)
    const accepted = files.filter((file) => file.size <= MAX_UPLOAD_BYTES)
    if (oversized.length > 0) {
      toast.danger(t('media.uploadSkippedTooLarge', { count: oversized.length }))
    }
    if (accepted.length === 0) return

    uploadingRef.current = true
    const controller = new AbortController()
    abortRef.current = controller
    setUpload({ total: accepted.length, done: 0, succeeded: 0, failed: 0 })

    const failedItems: FailedUpload[] = []
    let next = 0
    let succeeded = 0
    let failed = 0

    const worker = async () => {
      while (true) {
        if (controller.signal.aborted) return
        const index = next
        next += 1
        if (index >= accepted.length) return
        const file = accepted[index]
        try {
          await api.uploadMedia(file, file.name, { signal: controller.signal })
          succeeded += 1
        } catch (cause) {
          if (controller.signal.aborted) return
          failed += 1
          failedItems.push({
            id: uploadId(file),
            file,
            message: ApiError.from(cause, t('media.uploadFallback')).message,
          })
        } finally {
          if (!controller.signal.aborted) {
            setUpload((previous) =>
              previous
                ? {
                    total: previous.total,
                    done: Math.min(previous.done + 1, previous.total),
                    succeeded,
                    failed,
                  }
                : previous,
            )
          }
        }
      }
    }

    await Promise.all(
      Array.from({ length: Math.min(UPLOAD_CONCURRENCY, accepted.length) }, () => worker()),
    )

    const aborted = controller.signal.aborted
    const done = succeeded + failed
    uploadingRef.current = false
    abortRef.current = null
    setUpload(null)
    if (failedItems.length > 0) setFailures((previous) => [...previous, ...failedItems])

    if (aborted) {
      toast.danger(t('media.uploadCancelled', { done, total: accepted.length }))
    } else if (succeeded > 0) {
      toast.success(
        failed > 0
          ? t('media.uploadedPartial', { done: succeeded, failed })
          : t('media.uploaded', { count: succeeded }),
      )
    }

    if (succeeded > 0) {
      const reloaded = await load()
      if (!reloaded) toast.danger(t('media.error.reloadAfterUpload'))
    }
  }

  function cancelUpload() {
    abortRef.current?.abort()
  }

  async function retryFailure(item: FailedUpload) {
    if (uploadingRef.current || retryingId) return
    uploadingRef.current = true
    setRetryingId(item.id)
    setFailures((previous) => previous.filter((entry) => entry.id !== item.id))
    try {
      await api.uploadMedia(item.file, item.file.name)
      toast.success(t('media.uploaded', { count: 1 }))
      const reloaded = await load()
      if (!reloaded) toast.danger(t('media.error.reloadAfterUpload'))
    } catch (cause) {
      const message = ApiError.from(cause, t('media.uploadFallback')).message
      toast.danger(t('media.uploadFailed', { name: item.file.name, message }))
      setFailures((previous) => [...previous, { ...item, message }])
    } finally {
      uploadingRef.current = false
      setRetryingId(null)
    }
  }

  function handleFileInputChange(event: ChangeEvent<HTMLInputElement>) {
    const files = Array.from(event.target.files ?? [])
    // 清空 value，保证再次选择同一个文件仍会触发 change
    event.target.value = ''
    if (files.length > 0) void uploadFiles(files)
  }

  const normalizedQuery = query.trim().toLowerCase()

  const visibleAssets = useMemo(() => {
    if (!normalizedQuery) return assets
    return assets.filter(
      (asset) =>
        asset.filename.toLowerCase().includes(normalizedQuery) ||
        asset.mime.toLowerCase().includes(normalizedQuery),
    )
  }, [assets, normalizedQuery])

  const uploadBusy = isUploading || retryingId !== null
  const isBusy = deleting || uploadBusy
  const showEmpty = !loading && !error && assets.length === 0
  const showNoMatch = !loading && !error && assets.length > 0 && visibleAssets.length === 0
  const progressPercent = upload && upload.total > 0 ? Math.round((upload.done / upload.total) * 100) : 0

  function openDelete(asset: MediaAsset) {
    setDeleteError(null)
    setDeleteTarget(asset)
  }

  function closeDelete() {
    if (deleting) return
    setDeleteTarget(null)
    setDeleteError(null)
  }

  async function runDelete() {
    const target = deleteTarget
    if (!target || deleting) return
    setDeleting(true)
    setDeleteError(null)
    try {
      await api.deleteMedia(target.id)
      setAssets((previous) => previous.filter((item) => item.id !== target.id))
      setDeleteTarget(null)
      toast.success(t('media.deleted', { name: target.filename }))
    } catch (cause) {
      const message = ApiError.from(cause, t('media.error.deleteFallback')).message
      setDeleteError(message)
      toast.danger(message)
    } finally {
      setDeleting(false)
    }
  }

  return (
    <div>
      <PageHeader
        title={t('media.title')}
        description={t('media.description')}
        actions={
          <>
            <Button
              variant="primary"
              size="sm"
              className="h-10 gap-1.5 px-3"
              aria-label={t('media.uploadAria')}
              isDisabled={uploadBusy}
              onPress={openPicker}
            >
              <UploadIcon size={15} />
              {t('media.upload')}
            </Button>
            <Button
              variant="secondary"
              size="sm"
              className="h-10 gap-1.5 px-3"
              aria-label={t('media.refreshAria')}
              isPending={loading}
              onPress={() => void load()}
            >
              {loading ? null : <RefreshIcon size={14} />}
              {t('media.refresh')}
            </Button>
          </>
        }
      />

      <input
        ref={fileInputRef}
        type="file"
        multiple
        className="hidden"
        tabIndex={-1}
        aria-label={t('media.fileInputAria')}
        onChange={handleFileInputChange}
      />

      <div className="content-shell max-w-5xl py-6 md:py-8">
        <MediaUploadZone
          isUploading={uploadBusy}
          onPick={openPicker}
          onFiles={(files) => void uploadFiles(files)}
        />

        {upload ? (
          <div className="mt-3 rounded-lg border border-border bg-surface px-3 py-2">
            <div className="flex items-center justify-between gap-2 text-[11px] leading-4 text-muted">
              <span className="tabular" role="status" aria-live="polite">
                {t('media.progress.label', { done: upload.done, total: upload.total })}
              </span>
              <div className="flex items-center gap-2">
                <span className="tabular" aria-hidden="true">
                  {progressPercent}%
                </span>
                <Button
                  variant="ghost"
                  size="sm"
                  className="h-7 rounded-md px-2 text-[11px] text-muted hover:text-foreground"
                  onPress={cancelUpload}
                >
                  {t('media.uploadCancel')}
                </Button>
              </div>
            </div>
            <div
              role="progressbar"
              aria-label={t('media.progress.aria', { done: upload.done, total: upload.total })}
              aria-valuemin={0}
              aria-valuemax={upload.total}
              aria-valuenow={upload.done}
              className="mt-1.5 h-1.5 overflow-hidden rounded-full bg-surface-secondary"
            >
              <div
                className="h-full rounded-full bg-accent transition-[width] duration-300"
                style={{ width: `${progressPercent}%` }}
              />
            </div>
          </div>
        ) : null}

        {failures.length > 0 ? (
          <div className="mt-3 rounded-lg border border-danger/30 bg-danger/5 px-3 py-2.5">
            <p className="text-[12px] font-medium text-danger">
              {t('media.failed.title', { count: failures.length })}
            </p>
            <ul className="mt-2 flex flex-col gap-1.5">
              {failures.map((item) => (
                <li
                  key={item.id}
                  className="flex flex-wrap items-center justify-between gap-2 rounded-md bg-surface px-2.5 py-1.5"
                >
                  <div className="min-w-0 flex-1">
                    <p className="truncate text-[12px] text-foreground" title={item.file.name}>
                      {item.file.name}
                    </p>
                    <p className="truncate text-[11px] text-muted" title={item.message}>
                      {item.message}
                    </p>
                  </div>
                  <Button
                    variant="secondary"
                    size="sm"
                    className="h-8 shrink-0 rounded-md px-2.5 text-[11px]"
                    isPending={retryingId === item.id}
                    isDisabled={uploadBusy && retryingId !== item.id}
                    onPress={() => void retryFailure(item)}
                  >
                    {t('media.failed.retry')}
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        ) : null}

        <div className="mt-4 flex flex-wrap items-end justify-between gap-x-4 gap-y-2">
          <TextField
            value={query}
            onChange={setQuery}
            className="relative w-full sm:max-w-sm"
          >
            <Label className="sr-only">{t('media.searchAria')}</Label>
            <span
              aria-hidden="true"
              className="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 text-subtle"
            >
              <SearchIcon size={14} />
            </span>
            <Input
              placeholder={t('media.searchPlaceholder')}
              autoComplete="off"
              spellCheck={false}
              className="h-10 pl-8 pr-9 text-[13px]"
            />
            {query ? (
              <Button
                isIconOnly
                variant="ghost"
                size="sm"
                className="absolute right-0.5 top-1/2 h-10 w-10 -translate-y-1/2 text-muted"
                aria-label={t('media.noMatch.cta')}
                onPress={() => setQuery('')}
              >
                <XIcon size={14} aria-hidden="true" />
              </Button>
            ) : null}
          </TextField>

          {assets.length > 0 ? (
            <p className="text-xs text-muted tabular" aria-live="polite">
              {normalizedQuery
                ? t('media.filteredCount', { count: visibleAssets.length })
                : t('media.count', { count: assets.length })}
            </p>
          ) : null}
        </div>

        {error && !loading ? (
          <div className="mt-4 flex flex-col items-start gap-3">
            <Alert status="danger" className="w-full">
              <Alert.Indicator>
                <AlertIcon aria-hidden="true" className="size-4" />
              </Alert.Indicator>
              <Alert.Content>
                <Alert.Title>{t('media.error.title')}</Alert.Title>
                <Alert.Description>{error}</Alert.Description>
              </Alert.Content>
            </Alert>
            <Button
              variant="secondary"
              size="sm"
              className="h-10 gap-1.5 px-3"
              onPress={() => void load()}
            >
              <RefreshIcon size={14} />
              {t('media.error.retry')}
            </Button>
          </div>
        ) : null}

        {loading ? (
          <div className="mt-4">
            <MediaGridSkeleton />
          </div>
        ) : null}

        {showEmpty ? (
          <EmptyState className="mt-4 flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
            <span
              aria-hidden="true"
              className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
            >
              <UploadIcon size={20} />
            </span>
            <h2 className="text-sm font-semibold text-foreground">{t('media.empty.title')}</h2>
            <p className="max-w-sm text-xs leading-5 text-muted">{t('media.empty.description')}</p>
            <Button
              variant="primary"
              className="mt-1 h-10 gap-1.5 px-4"
              isDisabled={uploadBusy}
              onPress={openPicker}
            >
              <UploadIcon size={15} />
              {t('media.empty.cta')}
            </Button>
          </EmptyState>
        ) : null}

        {showNoMatch ? (
          <EmptyState className="mt-4 flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
            <span
              aria-hidden="true"
              className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
            >
              <SearchIcon size={20} />
            </span>
            <h2 className="text-sm font-semibold text-foreground">{t('media.noMatch.title')}</h2>
            <p className="max-w-sm text-xs leading-5 text-muted">{t('media.noMatch.description')}</p>
            <Button variant="secondary" className="mt-1 h-10 px-4" onPress={() => setQuery('')}>
              {t('media.noMatch.cta')}
            </Button>
          </EmptyState>
        ) : null}

        {!loading && !error && visibleAssets.length > 0 ? (
          <>
            <ul aria-label={t('media.listAria')} className={`mt-4 ${GRID_CLASS}`}>
              {visibleAssets.map((asset) => (
                <li key={asset.id} className="h-full">
                  <MediaCard
                    asset={asset}
                    isBusy={isBusy}
                    onPreview={setPreview}
                    onDelete={openDelete}
                  />
                </li>
              ))}
            </ul>

            {nextCursor ? (
              <div className="mt-4 flex justify-center">
                <Button
                  variant="secondary"
                  size="sm"
                  className="h-10 px-4"
                  isPending={loadingMore}
                  isDisabled={isBusy}
                  onPress={() => void loadMore()}
                >
                  {loadingMore ? null : <RefreshIcon size={13} />}
                  {t('media.loadMore')}
                </Button>
              </div>
            ) : null}
          </>
        ) : null}
      </div>

      <MediaPreviewModal asset={preview} onClose={() => setPreview(null)} />

      <DeleteMediaDialog
        asset={deleteTarget}
        isPending={deleting}
        error={deleteError}
        onOpenChange={(open) => {
          if (!open) closeDelete()
        }}
        onConfirm={() => void runDelete()}
      />
    </div>
  )
}
