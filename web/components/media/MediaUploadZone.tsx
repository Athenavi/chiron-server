'use client'

/**
 * 上传区：整块是原生 `<button>`，天然具备按钮语义与 Enter/Space 触发；
 * 拖拽进入时高亮，松开后把文件交给页面逐个上传。
 */

import { useRef, useState, type DragEvent } from 'react'
import { cn } from '@/lib/utils'
import { useI18n } from '@/lib/i18n'
import { UploadIcon } from './MediaIcons'

export interface MediaUploadZoneProps {
  /** 正在上传：禁用点击 / 拖放，避免并发批次互相覆盖进度。 */
  isUploading: boolean
  /** 点击或键盘激活时打开文件选择框。 */
  onPick: () => void
  /** 拖入的文件（可能为空数组）。 */
  onFiles: (files: File[]) => void
}

export function MediaUploadZone({ isUploading, onPick, onFiles }: MediaUploadZoneProps) {
  const { t } = useI18n()
  const [isDragging, setIsDragging] = useState(false)
  /** dragenter/dragleave 在子元素间会成对触发，用计数避免高亮闪烁。 */
  const dragDepth = useRef(0)

  function resetDrag() {
    dragDepth.current = 0
    setIsDragging(false)
  }

  function handleDragEnter(event: DragEvent<HTMLButtonElement>) {
    event.preventDefault()
    if (isUploading) return
    dragDepth.current += 1
    setIsDragging(true)
  }

  function handleDragOver(event: DragEvent<HTMLButtonElement>) {
    // 必须阻止默认行为，drop 才会触发
    event.preventDefault()
    if (isUploading) return
    event.dataTransfer.dropEffect = 'copy'
    setIsDragging(true)
  }

  function handleDragLeave(event: DragEvent<HTMLButtonElement>) {
    event.preventDefault()
    dragDepth.current = Math.max(0, dragDepth.current - 1)
    if (dragDepth.current === 0) setIsDragging(false)
  }

  function handleDrop(event: DragEvent<HTMLButtonElement>) {
    event.preventDefault()
    resetDrag()
    if (isUploading) return
    const files = Array.from(event.dataTransfer.files)
    if (files.length > 0) onFiles(files)
  }

  return (
    <button
      type="button"
      aria-label={t('media.dropzone.aria')}
      disabled={isUploading}
      onClick={onPick}
      onDragEnter={handleDragEnter}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
      className={cn(
        'flex w-full flex-col items-center justify-center gap-1.5 rounded-xl border border-dashed px-4 py-6 text-center transition-colors',
        'focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring',
        'disabled:cursor-not-allowed disabled:opacity-70',
        isDragging
          ? 'border-accent bg-accent-soft/50'
          : 'border-border bg-surface hover:border-border-strong hover:bg-surface-hover',
      )}
    >
      <span
        aria-hidden="true"
        className={cn(
          'flex size-10 items-center justify-center rounded-full',
          isDragging ? 'bg-accent text-accent-foreground' : 'bg-surface-secondary text-muted',
        )}
      >
        <UploadIcon size={18} />
      </span>
      <span className="text-[13px] font-medium text-foreground">
        {isUploading
          ? t('media.dropzone.disabled')
          : isDragging
            ? t('media.dropzone.active')
            : t('media.dropzone.title')}
      </span>
      {isUploading ? null : (
        <span className="max-w-md text-[11px] leading-5 text-muted">
          {t('media.dropzone.hint')}
        </span>
      )}
    </button>
  )
}
