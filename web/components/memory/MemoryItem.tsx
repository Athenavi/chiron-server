'use client'

/**
 * 单条记忆卡片：槽位徽标 + 作用域/来源/置信度 + 可展开内容 + 编辑/删除。
 *
 * - 内容是纯文本（可能含换行）：默认 3 行截断，超长时提供「展开」；
 * - 编辑/删除是图标按钮，带 aria-label（内容摘要），键盘可达；触控目标 ≥40px；
 * - 长作用域（session:<uuid>）单行省略，完整值放在 title 里，避免 360px 溢出；
 * - 不做数据请求，交给页面。
 */

import { useEffect, useId, useState } from 'react'
import { Button, Chip } from '@heroui/react'
import type { Memory } from '@/lib/types'
import { PencilIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import { cn } from '@/lib/utils'
import {
  confidenceLabel,
  contentPreview,
  scopeLabel,
  slotLabel,
  slotMeta,
  sourceLabel,
  updatedLabel,
  updatedTitle,
} from './memory-meta'

export interface MemoryItemProps {
  memory: Memory
  /** 有其它写操作进行中时禁用本行按钮。 */
  isBusy?: boolean
  onEdit: (memory: Memory) => void
  onDelete: (memory: Memory) => void
}

/** 超过这个长度 / 行数就折叠，避免长摘要撑满列表。 */
const CLAMP_CHARS = 160
const CLAMP_LINES = 3

export function MemoryItem({ memory, isBusy = false, onEdit, onDelete }: MemoryItemProps) {
  const { t, locale } = useI18n()
  const [expanded, setExpanded] = useState(false)
  const contentId = useId()

  // 内容被改短后不需要保留展开态
  useEffect(() => {
    if (memory.content.length <= CLAMP_CHARS) setExpanded(false)
  }, [memory.content])

  const meta = slotMeta(memory.slot)
  const long =
    memory.content.length > CLAMP_CHARS || memory.content.split('\n').length > CLAMP_LINES
  const preview = contentPreview(memory.content)

  return (
    <article className="rounded-xl border border-border bg-surface p-3.5 shadow-xs transition-colors hover:border-border-strong sm:p-4">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-2.5 gap-y-1.5">
            <Chip size="sm" variant="soft" color={meta.color}>
              {slotLabel(memory.slot, t)}
            </Chip>
            <span className="max-w-[12rem] truncate text-[11px] text-muted" title={memory.scope}>
              {t('memory.list.scope', { scope: scopeLabel(memory.scope, t) })}
            </span>
            <span className="text-[11px] text-muted">
              {t('memory.list.source', { source: sourceLabel(memory.source, t) })}
            </span>
            <span className="text-[11px] text-muted tabular">
              {t('memory.list.confidence', { confidence: confidenceLabel(memory.confidence) })}
            </span>
          </div>

          <p
            id={contentId}
            className={cn(
              'mt-2 whitespace-pre-wrap break-words text-[13px] leading-6 text-foreground',
              long && !expanded && 'line-clamp-3',
            )}
          >
            {memory.content}
          </p>

          <div className="mt-1.5 flex flex-wrap items-center gap-x-3 gap-y-1">
            {long ? (
              <Button
                variant="ghost"
                size="sm"
                aria-expanded={expanded}
                aria-controls={contentId}
                className="h-9 px-2 text-[11px] md:h-7 md:px-1.5"
                onPress={() => setExpanded((value) => !value)}
              >
                {expanded ? t('memory.list.collapse') : t('memory.list.expand')}
              </Button>
            ) : null}
            <span
              className="text-[11px] text-subtle"
              title={updatedTitle(memory.updated_at, locale)}
            >
              {t('memory.list.updatedAt', { time: updatedLabel(memory.updated_at, t, locale) })}
            </span>
          </div>
        </div>

        <div className="flex shrink-0 items-center gap-0.5">
          <Button
            isIconOnly
            size="sm"
            variant="ghost"
            aria-label={t('memory.list.editAria', { preview })}
            isDisabled={isBusy}
            onPress={() => onEdit(memory)}
            className="h-10 w-10 text-muted hover:text-foreground md:h-8 md:w-8"
          >
            <PencilIcon size={15} />
          </Button>
          <Button
            isIconOnly
            size="sm"
            variant="ghost"
            aria-label={t('memory.list.deleteAria', { preview })}
            isDisabled={isBusy}
            onPress={() => onDelete(memory)}
            className="h-10 w-10 text-muted hover:text-danger md:h-8 md:w-8"
          >
            <TrashIcon size={15} />
          </Button>
        </div>
      </div>
    </article>
  )
}
