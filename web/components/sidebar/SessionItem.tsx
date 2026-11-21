'use client'

/**
 * 侧栏里的单个会话项。
 *
 * - 标题单行省略，当前项高亮（`aria-current="page"`）；
 * - hover / 键盘聚焦时右侧出现操作菜单（重命名 / 归档 / 删除）；
 * - 重命名是行内编辑：Enter 保存、Esc 取消、失焦保存；空标题回落「未命名会话」。
 *
 * 这里只负责呈现与交互，数据请求由 `SessionSidebar` 统一处理。
 */

import { useRef, useState } from 'react'
import Link from 'next/link'
import type { Key } from 'react'
import { Button, Dropdown, Input, TextField } from '@heroui/react'
import type { ChatSession } from '@/lib/types'
import { useI18n, type Locale } from '@/lib/i18n'
import {
  ArchiveIcon,
  DownloadIcon,
  MessageIcon,
  MoreIcon,
  PencilIcon,
  ShareIcon,
  TrashIcon,
} from '@/components/ui-bridge/icons'
import { cn } from '@/lib/utils'

/**
 * 相对时间：`lib/utils` 的 `formatRelative` 是固定中文，属于其他代理的文件，
 * 这里按当前语言重写一份（口径保持一致：刚刚 / N 分钟前 / N 小时前 / 昨天 / 日期）。
 */
function relativeLabel(value: string, locale: Locale, t: (key: string, vars?: Record<string, string | number>) => string): string {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  const diff = Date.now() - date.getTime()
  const minute = 60_000
  const hour = 60 * minute
  const day = 24 * hour
  if (diff < minute) return t('chat.relative.justNow')
  if (diff < hour) return t('chat.relative.minutes', { n: Math.floor(diff / minute) })
  if (diff < day) return t('chat.relative.hours', { n: Math.floor(diff / hour) })
  if (diff < 2 * day) return t('chat.relative.yesterday')
  return date.toLocaleDateString(locale === 'en' ? 'en-US' : 'zh-CN', {
    month: '2-digit',
    day: '2-digit',
  })
}

export interface SessionItemProps {
  session: ChatSession
  active: boolean
  /** 该会话正在执行归档 / 删除，菜单与编辑暂时禁用。 */
  busy?: boolean
  onRename: (session: ChatSession, title: string) => Promise<void>
  onArchive: (session: ChatSession) => void
  onExport: (session: ChatSession) => void
  onShare: (session: ChatSession) => void
  /** 删除在行内二次确认；失败时由父级 toast，行内保持确认态。 */
  onDelete: (session: ChatSession) => Promise<void>
}

export function SessionItem({
  session,
  active,
  busy = false,
  onRename,
  onArchive,
  onExport,
  onShare,
  onDelete,
}: SessionItemProps) {
  const { t, locale } = useI18n()
  const title = session.title?.trim() || t('chat.untitled')
  const pending =
    session.pending_approvals ??
    (session.active_run?.status === 'waiting_approval' ? 1 : 0)
  const [renaming, setRenaming] = useState(false)
  const [confirmingDelete, setConfirmingDelete] = useState(false)
  const [deleting, setDeleting] = useState(false)
  const [draft, setDraft] = useState(title)
  const [saving, setSaving] = useState(false)
  // Esc 取消后浏览器会补一次 blur：用它跳过「失焦保存」
  const cancelledRef = useRef(false)

  function startRename() {
    setDraft(title)
    cancelledRef.current = false
    setRenaming(true)
  }

  function cancelRename() {
    cancelledRef.current = true
    setRenaming(false)
  }

  async function commitRename() {
    if (cancelledRef.current) {
      cancelledRef.current = false
      setRenaming(false)
      return
    }
    if (saving) return
    const next = draft.trim()
    if (!next || next === title) {
      setRenaming(false)
      return
    }
    setSaving(true)
    try {
      await onRename(session, next)
      setRenaming(false)
    } catch {
      // 失败时保持编辑态，由父级负责提示（toast）
    } finally {
      setSaving(false)
    }
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (event.key === 'Enter') {
      event.preventDefault()
      void commitRename()
      return
    }
    if (event.key === 'Escape') {
      event.preventDefault()
      cancelRename()
    }
  }

  function handleAction(key: Key) {
    if (key === 'rename') {
      setConfirmingDelete(false)
      startRename()
      return
    }
    if (key === 'export') {
      onExport(session)
      return
    }
    if (key === 'share') {
      onShare(session)
      return
    }
    if (key === 'archive') {
      onArchive(session)
      return
    }
    if (key === 'delete') {
      setRenaming(false)
      setConfirmingDelete(true)
    }
  }

  async function confirmDelete() {
    if (deleting) return
    setDeleting(true)
    try {
      await onDelete(session)
      setConfirmingDelete(false)
    } catch {
      // 失败时保留确认态，由父级 toast 提示
    } finally {
      setDeleting(false)
    }
  }

  return (
    <div
      className={cn(
        'group/item relative flex items-center rounded-md',
        active ? 'bg-sidebar-active' : 'hover:bg-sidebar-hover',
      )}
    >
      {confirmingDelete ? (
        <div className="flex min-w-0 flex-1 items-center gap-1.5 px-2 py-1.5">
          <span className="min-w-0 flex-1 truncate text-[12px] leading-5 text-sidebar-foreground/90">
            {t('chat.sidebar.deleteConfirm', { title })}
          </span>
          <Button
            variant="ghost"
            size="sm"
            isDisabled={deleting}
            onPress={() => setConfirmingDelete(false)}
            autoFocus
            className="h-10 shrink-0 rounded-md px-2 text-[12px] text-sidebar-muted hover:text-sidebar-foreground md:h-8"
          >
            {t('common.cancel')}
          </Button>
          <Button
            variant="danger"
            size="sm"
            isPending={deleting}
            onPress={() => void confirmDelete()}
            className="h-10 shrink-0 rounded-md px-2 text-[12px] md:h-8"
          >
            {t('common.delete')}
          </Button>
        </div>
      ) : renaming ? (
        <div className="flex min-w-0 flex-1 items-center gap-1 px-1.5 py-1">
          <TextField
            aria-label={t('chat.header.renameAria', { title })}
            isDisabled={saving}
            className="min-w-0 flex-1"
          >
            <Input
              autoFocus
              value={draft}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={handleKeyDown}
              onBlur={() => void commitRename()}
              className="h-7 min-w-0 border-sidebar-border bg-sidebar-elevated px-2 text-[13px] text-sidebar-foreground shadow-none"
            />
          </TextField>
        </div>
      ) : (
        <Link
          href={`/chat/${session.id}`}
          aria-current={active ? 'page' : undefined}
          title={session.model ? `${title} · ${session.model}` : title}
          className={cn(
            'flex min-w-0 flex-1 items-center gap-2 rounded-md py-2.5 pe-12 ps-2 text-[13px] leading-5 md:pe-10',
            'transition-colors focus-visible:outline-2 focus-visible:outline-offset-[-2px] focus-visible:outline-ring',
            active
              ? 'font-medium text-sidebar-foreground'
              : 'text-sidebar-foreground/85 group-hover/item:text-sidebar-foreground',
          )}
        >
          <MessageIcon
            size={14}
            className={cn('shrink-0', active ? 'text-sidebar-foreground' : 'text-sidebar-muted')}
          />
          <span className="min-w-0 flex-1 truncate">{title}</span>
          {pending > 0 ? (
            <span
              title={t('chat.sidebar.pendingAria', { n: pending })}
              aria-label={t('chat.sidebar.pendingAria', { n: pending })}
              className="shrink-0 rounded-full bg-warning/20 px-1.5 py-0.5 text-[10px] font-semibold tabular-nums text-warning"
            >
              {pending}
            </span>
          ) : null}
          <span
            aria-hidden="true"
            className="shrink-0 text-[11px] tabular-nums text-sidebar-muted"
          >
            {relativeLabel(session.updated_at, locale, t)}
          </span>
        </Link>
      )}

      {!renaming && !confirmingDelete ? (
        <div
          className={cn(
            'absolute right-1 top-1/2 -translate-y-1/2 transition-opacity',
            'md:pointer-events-none md:opacity-0 md:group-focus-within/item:pointer-events-auto',
            'md:group-hover/item:pointer-events-auto md:group-focus-within/item:opacity-100 md:group-hover/item:opacity-100',
          )}
        >
          <Dropdown>
            <Dropdown.Trigger
              aria-label={t('chat.sidebar.actionsAria', { title })}
              isDisabled={busy}
              className="inline-flex h-10 w-10 items-center justify-center rounded-md text-sidebar-muted transition-colors hover:bg-sidebar-active hover:text-sidebar-foreground data-[focus-visible]:outline-2 data-[focus-visible]:outline-offset-1 data-[focus-visible]:outline-ring md:h-8 md:w-8"
            >
              <MoreIcon size={15} />
            </Dropdown.Trigger>
            <Dropdown.Popover placement="bottom end">
              <Dropdown.Menu
                aria-label={t('chat.sidebar.actionsAria', { title })}
                onAction={handleAction}
              >
                <Dropdown.Item id="rename" textValue={t('chat.sidebar.rename')}>
                  <PencilIcon size={14} />
                  <span className="truncate">{t('chat.sidebar.rename')}</span>
                </Dropdown.Item>
                <Dropdown.Item id="export" textValue={t('chat.sessionActions.export')}>
                  <DownloadIcon size={14} />
                  <span className="truncate">{t('chat.sessionActions.export')}</span>
                </Dropdown.Item>
                <Dropdown.Item id="share" textValue={t('chat.sessionActions.share')}>
                  <ShareIcon size={14} />
                  <span className="truncate">{t('chat.sessionActions.share')}</span>
                </Dropdown.Item>
                <Dropdown.Item id="archive" textValue={t('chat.sidebar.archive')}>
                  <ArchiveIcon size={14} />
                  <span className="truncate">{t('chat.sidebar.archive')}</span>
                </Dropdown.Item>
                <Dropdown.Item
                  id="delete"
                  textValue={t('chat.sidebar.delete')}
                  className="text-danger"
                >
                  <TrashIcon size={14} />
                  <span className="truncate">{t('chat.sidebar.delete')}</span>
                </Dropdown.Item>
              </Dropdown.Menu>
            </Dropdown.Popover>
          </Dropdown>
        </div>
      ) : null}
    </div>
  )
}

export default SessionItem
