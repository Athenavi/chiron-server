'use client'

/**
 * 聊天页顶栏。
 *
 * 左侧：移动端菜单（通过 `chiron:toggle-sidebar` 事件让 layout 打开抽屉）+ 可编辑标题；
 * 右侧：运行状态、模型选择、主题切换、更多菜单（复制会话 ID / 归档）。
 *
 * 标题编辑：Enter 保存、Esc 取消、失焦保存；空标题回落「未命名会话」。
 * 保存调用 `onRename`，模型切换调用 `onModelChange`，两者都由聊天页负责落库。
 */

import { useRef, useState } from 'react'
import { useRouter } from 'next/navigation'
import type { Key } from 'react'
import { Button, Chip, Dropdown, Input, TextField, toast } from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError, type ChatSession } from '@/lib/types'
import { useI18n } from '@/lib/i18n'
import { ThemeSwitcher } from '@/components/theme/ThemeSwitcher'
import { ModelPicker } from '@/components/chat/ModelPicker'
import { DisplaySettingsDialog } from '@/components/chat/DisplaySettingsDialog'
import { SessionRunsPanel } from '@/components/chat/SessionRunsPanel'
import { exportSession, shareSession } from '@/lib/session-export'
import {
  ArchiveIcon,
  ChartIcon,
  CopyIcon,
  DownloadIcon,
  MenuIcon,
  MoreIcon,
  PencilIcon,
  ShareIcon,
  SlidersIcon,
} from '@/components/ui-bridge/icons'
import { cn } from '@/lib/utils'

/**
 * 移动端抽屉开关事件名。
 * `app/(app)/layout.tsx` 监听同一个常量；`ChatHeader` 里只用 `window.dispatchEvent`，
 * 不依赖 layout 提供 context（聊天页的 props 契约保持最小）。
 */
export const TOGGLE_SIDEBAR_EVENT = 'chiron:toggle-sidebar'

export interface ChatHeaderProps {
  session: ChatSession | null
  running: boolean
  /** 可选：运行正在等待审批时展示「等待审批」（契约里的 4 个 props 不变）。 */
  waitingApproval?: boolean
  onRename: (title: string) => void | Promise<void>
  onModelChange: (model: string | null) => void | Promise<void>
}

export function ChatHeader({
  session,
  running,
  waitingApproval = false,
  onRename,
  onModelChange,
}: ChatHeaderProps) {
  const router = useRouter()
  const { t } = useI18n()
  // 标题回落也在本地化：`sessionTitle` 的回落文案是固定中文，不能直接用于英文界面
  const title = session?.title?.trim() || t('chat.untitled')
  const [editing, setEditing] = useState(false)
  const [draft, setDraft] = useState(title)
  const [saving, setSaving] = useState(false)
  const [archiving, setArchiving] = useState(false)
  const [exporting, setExporting] = useState(false)
  const [sharing, setSharing] = useState(false)
  const [displayOpen, setDisplayOpen] = useState(false)
  const [runsOpen, setRunsOpen] = useState(false)
  const cancelledRef = useRef(false)
  const menuButtonRef = useRef<HTMLButtonElement | null>(null)

  function startEditing() {
    if (!session) return
    setDraft(title)
    cancelledRef.current = false
    setEditing(true)
  }

  function cancelEditing() {
    cancelledRef.current = true
    setEditing(false)
  }

  async function commitEditing() {
    if (cancelledRef.current) {
      cancelledRef.current = false
      setEditing(false)
      return
    }
    if (saving) return
    if (!session) {
      setEditing(false)
      return
    }
    // 空标题按「未命名会话」保存，而不是拒绝
    const next = draft.trim() || t('chat.untitled')
    if (next === title) {
      setEditing(false)
      return
    }
    setSaving(true)
    try {
      await onRename(next)
      setEditing(false)
    } catch {
      // 失败提示由聊天页负责（它会 toast 后 reject），这里保留编辑态让用户重试
    } finally {
      setSaving(false)
    }
  }

  function handleTitleKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (event.key === 'Enter') {
      event.preventDefault()
      void commitEditing()
      return
    }
    if (event.key === 'Escape') {
      event.preventDefault()
      cancelEditing()
    }
  }

  function toggleSidebar() {
    if (typeof window === 'undefined') return
    // detail 带上触发按钮：layout 关闭抽屉后把焦点还给它
    window.dispatchEvent(
      new CustomEvent(TOGGLE_SIDEBAR_EVENT, { detail: { trigger: menuButtonRef.current } }),
    )
  }

  async function copySessionId() {
    if (!session) return
    try {
      await navigator.clipboard.writeText(session.id)
      toast.success(t('chat.header.copyIdSuccess'))
    } catch {
      toast.danger(t('chat.header.copyIdFailed'))
    }
  }

  async function archiveSession() {
    if (!session || archiving) return
    setArchiving(true)
    try {
      await api.updateSession(session.id, { archived: true })
      toast.success(t('chat.header.archiveSuccess'))
      router.push('/sessions')
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('chat.header.archiveFailed')).message)
    } finally {
      setArchiving(false)
    }
  }

  async function handleExport() {
    if (!session || exporting) return
    setExporting(true)
    try {
      const count = await exportSession(session)
      toast.success(t('chat.sessionActions.exported', { n: count }))
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('chat.sessionActions.exportFailed')).message)
    } finally {
      setExporting(false)
    }
  }

  async function handleShare() {
    if (!session || sharing) return
    setSharing(true)
    try {
      const result = await shareSession(session)
      if (result === 'shared') toast.success(t('chat.sessionActions.shareSuccess'))
      else if (result === 'copied') toast.success(t('chat.sessionActions.shareCopied'))
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('chat.sessionActions.shareFailed')).message)
    } finally {
      setSharing(false)
    }
  }

  function handleMoreAction(key: Key) {
    if (key === 'copy-id') {
      void copySessionId()
      return
    }
    if (key === 'export') {
      void handleExport()
      return
    }
    if (key === 'share') {
      void handleShare()
      return
    }
    if (key === 'display') {
      setDisplayOpen(true)
      return
    }
    if (key === 'runs') {
      setRunsOpen(true)
      return
    }
    if (key === 'archive') void archiveSession()
  }

  const statusLabel = waitingApproval
    ? t('chat.status.waitingApproval')
    : running
      ? t('chat.status.running')
      : t('chat.status.idle')

  return (
    <>
      <header className="flex h-14 shrink-0 items-center gap-1.5 border-b border-border bg-background px-2 md:px-4">
      <Button
        ref={menuButtonRef}
        variant="ghost"
        isIconOnly
        aria-label={t('common.openSidebar')}
        onPress={toggleSidebar}
        className="-ml-0.5 h-10 w-10 shrink-0 rounded-md text-muted hover:text-foreground md:hidden"
      >
        <MenuIcon size={18} />
      </Button>

      <div className="flex min-w-0 flex-1 items-center">
        {editing ? (
          <TextField
            aria-label={t('chat.header.titleLabel')}
            isDisabled={saving}
            className="w-full max-w-md"
          >
            <Input
              autoFocus
              value={draft}
              maxLength={120}
              placeholder={t('chat.untitled')}
              onChange={(event) => setDraft(event.target.value)}
              onKeyDown={handleTitleKeyDown}
              onBlur={() => void commitEditing()}
              onFocus={(event) => event.currentTarget.select()}
              className="h-9 text-sm font-semibold"
            />
          </TextField>
        ) : (
          <button
            type="button"
            disabled={!session}
            aria-label={
              session ? t('chat.header.renameAria', { title }) : t('chat.header.titleLabel')
            }
            onClick={startEditing}
            className={cn(
              'group inline-flex min-w-0 max-w-full items-center gap-1.5 rounded-md px-1.5 py-1 text-left',
              'transition-colors hover:bg-surface-hover disabled:cursor-default disabled:hover:bg-transparent',
              'focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring',
            )}
          >
            <h1 className="min-w-0 truncate text-sm font-semibold tracking-tight text-foreground">
              {session ? title : t('chat.header.session')}
            </h1>
            {session ? (
              <PencilIcon
                size={13}
                className="shrink-0 text-muted opacity-0 transition-opacity group-hover:opacity-100 group-focus-visible:opacity-100"
              />
            ) : null}
          </button>
        )}
      </div>

      <div className="flex shrink-0 items-center gap-1.5">
        <Chip
          size="sm"
          variant="soft"
          color={waitingApproval ? 'warning' : running ? 'success' : 'default'}
          className="hidden sm:inline-flex"
          aria-label={t('chat.status.aria', { status: statusLabel })}
        >
          <span
            aria-hidden="true"
            className={cn(
              'size-1.5 rounded-full',
              waitingApproval
                ? 'animate-pulse bg-warning'
                : running
                  ? 'animate-pulse bg-success'
                  : 'bg-muted',
            )}
          />
          {statusLabel}
        </Chip>

        <ModelPicker
          model={session?.model ?? null}
          isDisabled={!session || saving || archiving}
          onModelChange={onModelChange}
        />

        <Button
          variant="ghost"
          isIconOnly
          aria-label={t('chat.runs.open')}
          isDisabled={!session}
          onPress={() => setRunsOpen(true)}
          className="hidden h-9 w-9 rounded-full text-muted hover:bg-surface-hover hover:text-foreground sm:inline-flex"
        >
          <ChartIcon size={16} />
        </Button>

        <div className="hidden sm:block">
          <ThemeSwitcher />
        </div>

        <Dropdown>
          <Dropdown.Trigger
            aria-label={t('chat.header.more')}
            isDisabled={!session}
            className="inline-flex h-10 w-10 items-center justify-center rounded-full text-muted transition-colors hover:bg-surface-hover hover:text-foreground data-[focus-visible]:outline-2 data-[focus-visible]:outline-offset-1 data-[focus-visible]:outline-ring md:h-9 md:w-9"
          >
            <MoreIcon size={16} />
          </Dropdown.Trigger>
          <Dropdown.Popover placement="bottom end">
            <Dropdown.Menu aria-label={t('chat.header.more')} onAction={handleMoreAction}>
              <Dropdown.Item id="copy-id" textValue={t('chat.header.copyId')}>
                <CopyIcon size={14} />
                <span className="truncate">{t('chat.header.copyId')}</span>
              </Dropdown.Item>
              <Dropdown.Item id="export" textValue={t('chat.sessionActions.export')}>
                <DownloadIcon size={14} />
                <span className="truncate">{t('chat.sessionActions.export')}</span>
              </Dropdown.Item>
              <Dropdown.Item id="share" textValue={t('chat.sessionActions.share')}>
                <ShareIcon size={14} />
                <span className="truncate">{t('chat.sessionActions.share')}</span>
              </Dropdown.Item>
              <Dropdown.Item id="display" textValue={t('chat.display.title')}>
                <SlidersIcon size={14} />
                <span className="truncate">{t('chat.display.title')}</span>
              </Dropdown.Item>
              <Dropdown.Item id="runs" textValue={t('chat.runs.open')}>
                <ChartIcon size={14} />
                <span className="truncate">{t('chat.runs.open')}</span>
              </Dropdown.Item>
              <Dropdown.Item id="archive" textValue={t('chat.header.archive')}>
                <ArchiveIcon size={14} />
                <span className="truncate">{t('chat.header.archive')}</span>
              </Dropdown.Item>
            </Dropdown.Menu>
          </Dropdown.Popover>
        </Dropdown>
      </div>
      </header>

      <DisplaySettingsDialog isOpen={displayOpen} onOpenChange={setDisplayOpen} />
      <SessionRunsPanel
        session={session}
        isOpen={runsOpen}
        onOpenChange={setRunsOpen}
      />
    </>
  )
}

export default ChatHeader
