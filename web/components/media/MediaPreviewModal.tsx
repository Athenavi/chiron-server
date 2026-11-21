'use client'

/**
 * 图片预览弹窗：大图自适应（`object-contain`，最高 70vh），提供下载与关闭。
 * 图片加载失败时在弹窗内给出提示，不阻塞下载入口。
 */

import { useState } from 'react'
import { Button, Modal } from '@heroui/react'
import { XIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import { api } from '@/lib/api'
import type { MediaAsset } from '@/lib/types'
import { formatDateTime } from '@/lib/utils'
import { DownloadIcon, ImageIcon } from './MediaIcons'
import { formatBytes } from './media-utils'

function PreviewImage({ asset }: { asset: MediaAsset }) {
  const { t } = useI18n()
  const [failed, setFailed] = useState(false)

  if (failed) {
    return (
      <div className="flex flex-col items-center gap-2 rounded-lg bg-surface-secondary px-4 py-14 text-center">
        <ImageIcon size={26} className="text-muted" />
        <p className="text-[12px] text-muted">{t('media.previewFailed')}</p>
      </div>
    )
  }

  return (
    // eslint-disable-next-line @next/next/no-img-element -- 同源 / object URL 预览，next/image 不适用
    <img
      src={api.mediaUrl(asset.id)}
      alt={t('media.imageAlt', { name: asset.filename })}
      className="mx-auto max-h-[70vh] w-full rounded-lg bg-surface-secondary object-contain"
      onError={() => setFailed(true)}
    />
  )
}

export interface MediaPreviewModalProps {
  /** 为空表示关闭。 */
  asset: MediaAsset | null
  onClose: () => void
}

export function MediaPreviewModal({ asset, onClose }: MediaPreviewModalProps) {
  const { t, locale } = useI18n()

  return (
    <Modal
      isOpen={asset !== null}
      onOpenChange={(open) => {
        if (!open) onClose()
      }}
    >
      <Modal.Backdrop>
        <Modal.Container placement="center" scroll="inside">
          <Modal.Dialog
            aria-label={asset ? t('media.previewAria', { name: asset.filename }) : t('media.previewTitle')}
            className="w-full max-w-3xl"
          >
            <Modal.Header>
              <Modal.Icon className="bg-surface-secondary text-muted">
                <ImageIcon size={18} />
              </Modal.Icon>
              <div className="min-w-0 flex-1">
                <Modal.Heading className="truncate">
                  {asset ? asset.filename : t('media.previewTitle')}
                </Modal.Heading>
                {asset ? (
                  <p className="mt-0.5 truncate text-[11px] text-muted">
                    <span className="tabular">{formatBytes(asset.bytes)}</span>
                    <span aria-hidden="true"> · </span>
                    <span>{formatDateTime(asset.created_at, locale)}</span>
                  </p>
                ) : null}
              </div>
              <Button
                isIconOnly
                variant="ghost"
                size="sm"
                className="h-10 w-10 shrink-0"
                aria-label={t('media.previewClose')}
                onPress={onClose}
              >
                <XIcon size={16} />
              </Button>
            </Modal.Header>

            <Modal.Body>{asset ? <PreviewImage key={asset.id} asset={asset} /> : null}</Modal.Body>

            <Modal.Footer>
              {asset ? (
                <a
                  href={api.mediaUrl(asset.id, true)}
                  download={asset.filename}
                  aria-label={t('media.downloadAria', { name: asset.filename })}
                  className="inline-flex h-10 items-center gap-1.5 rounded-lg border border-border bg-surface px-3 text-[13px] font-medium text-foreground transition-colors hover:bg-surface-hover focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
                >
                  <DownloadIcon size={15} />
                  {t('media.download')}
                </a>
              ) : null}
              <Button variant="primary" className="h-10 px-4" onPress={onClose} autoFocus>
                {t('media.close')}
              </Button>
            </Modal.Footer>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  )
}
