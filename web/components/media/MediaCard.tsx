'use client'

/**
 * 文件卡片：缩略图 / 类型图标 + 文件名、MIME、大小与上传时间 + 操作。
 *
 * - 图片用 `<img loading="lazy">` + `object-cover` 铺满缩略区，加载失败回落到文件图标；
 * - 非图片按扩展名 / MIME 显示类型图标，并给出扩展名徽标；
 * - 卡片的异步状态由页面下发，自身只保留「图片是否加载失败」这一视觉状态。
 */

import { useState } from 'react'
import { Button } from '@heroui/react'
import { EyeIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import { api } from '@/lib/api'
import type { MediaAsset } from '@/lib/types'
import { formatDateTime } from '@/lib/utils'
import { DownloadIcon } from './MediaIcons'
import { MediaTypeIcon } from './MediaTypeIcon'
import { extensionOf, formatBytes } from './media-utils'

export interface MediaCardProps {
  asset: MediaAsset
  /** 有删除 / 上传在排队时禁用本卡操作。 */
  isBusy?: boolean
  onPreview: (asset: MediaAsset) => void
  onDelete: (asset: MediaAsset) => void
}

/** 触控目标：卡片操作按钮统一 40px 高。 */
const ACTION_BUTTON_CLASS =
  'h-10 min-w-0 gap-1.5 rounded-lg px-2.5 text-[12px] font-medium'

export function MediaCard({ asset, isBusy = false, onPreview, onDelete }: MediaCardProps) {
  const { t, locale } = useI18n()
  const [failedUrl, setFailedUrl] = useState<string | null>(null)

  const isImage = asset.kind === 'image'
  const imageFailed = failedUrl === asset.url
  const ext = extensionOf(asset.filename)
  const downloadHref = api.mediaUrl(asset.id, true)

  return (
    <article className="flex h-full flex-col overflow-hidden rounded-xl border border-border bg-surface shadow-xs transition-colors hover:border-border-strong">
      <div className="relative flex aspect-[4/3] w-full items-center justify-center overflow-hidden bg-surface-secondary">
        {isImage && !imageFailed ? (
          // eslint-disable-next-line @next/next/no-img-element -- 同源 / object URL 缩略图，无需 next/image 优化
          <img
            src={api.mediaUrl(asset.id)}
            alt={t('media.imageAlt', { name: asset.filename })}
            loading="lazy"
            decoding="async"
            className="h-full w-full object-cover"
            onError={() => setFailedUrl(asset.url)}
          />
        ) : (
          <div className="flex flex-col items-center gap-2 px-3 text-center">
            {isImage ? (
              <MediaTypeIcon filename={asset.filename} mime={asset.mime} size={34} />
            ) : (
              <span className="flex size-14 items-center justify-center rounded-xl border border-border bg-surface">
                <MediaTypeIcon filename={asset.filename} mime={asset.mime} size={26} />
              </span>
            )}
            {ext ? (
              <span className="rounded-full bg-surface-secondary px-2 py-0.5 font-mono text-[10px] uppercase tracking-wide text-muted">
                {ext}
              </span>
            ) : null}
            {isImage && imageFailed ? (
              <span className="text-[10px] leading-4 text-subtle">{t('media.previewFailed')}</span>
            ) : null}
          </div>
        )}
      </div>

      <div className="flex min-w-0 flex-1 flex-col gap-2 p-3">
        <h2
          className="truncate text-[13px] font-semibold leading-5 text-foreground"
          title={asset.filename}
        >
          {asset.filename}
        </h2>

        <p className="truncate font-mono text-[11px] leading-4 text-muted" title={asset.mime}>
          {asset.mime || 'application/octet-stream'}
        </p>

        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px] leading-4 text-subtle">
          <span className="tabular">{formatBytes(asset.bytes)}</span>
          <span aria-hidden="true">·</span>
          <span className="tabular" title={formatDateTime(asset.created_at, locale)}>
            {formatDateTime(asset.created_at, locale)}
          </span>
        </div>

        <div className="mt-auto flex items-center gap-1.5 pt-2">
          {isImage ? (
            <Button
              variant="secondary"
              size="sm"
              className={ACTION_BUTTON_CLASS}
              aria-label={t('media.previewAria', { name: asset.filename })}
              isDisabled={isBusy}
              onPress={() => onPreview(asset)}
            >
              <EyeIcon size={14} />
              <span className="truncate">{t('media.preview')}</span>
            </Button>
          ) : null}

          <a
            href={downloadHref}
            download={asset.filename}
            aria-label={t('media.downloadAria', { name: asset.filename })}
            title={t('media.download')}
            className={
              'inline-flex items-center justify-center gap-1.5 rounded-lg border border-border bg-surface px-2.5 text-[12px] font-medium text-foreground transition-colors hover:bg-surface-hover focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring ' +
              (isImage ? 'h-10 w-10 shrink-0' : 'h-10 min-w-0 flex-1')
            }
          >
            <DownloadIcon size={14} />
            {isImage ? null : <span className="truncate">{t('media.download')}</span>}
          </a>

          <Button
            isIconOnly
            variant="ghost"
            size="sm"
            className="h-10 w-10 shrink-0 text-danger"
            aria-label={t('media.deleteAria', { name: asset.filename })}
            isDisabled={isBusy}
            onPress={() => onDelete(asset)}
          >
            <TrashIcon size={15} />
          </Button>
        </div>
      </div>
    </article>
  )
}
