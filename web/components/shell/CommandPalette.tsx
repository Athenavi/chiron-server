'use client'

/**
 * 命令面板（Cmd/Ctrl+K）。
 *
 * 支持页面跳转、会话跳转、新建会话与当前会话切模型；点击遮罩/Esc 关闭，
 * 不强制选择。模型切换通过 `chiron:change-model` 事件交给聊天页的既有状态机，
 * 避免两处直接写 API。
 */

import { useCallback, useEffect, useMemo, useRef, useState, type KeyboardEvent } from 'react'
import { usePathname, useRouter } from 'next/navigation'
import { toast } from '@heroui/react'
import { api } from '@/lib/api'
import { useAuth } from '@/components/auth/AuthProvider'
import { useI18n } from '@/lib/i18n'
import { ApiError, type ChatSession, type TenantModel } from '@/lib/types'
import {
  ArchiveIcon,
  ChartIcon,
  CloudIcon,
  CubeIcon,
  MessageIcon,
  PlusIcon,
  SettingsIcon,
  ShieldIcon,
  SparkIcon,
  UserIcon,
  WrenchIcon,
} from '@/components/ui-bridge/icons'
import { cn } from '@/lib/utils'

export const CHANGE_MODEL_EVENT = 'chiron:change-model'

type CommandGroup = 'pages' | 'sessions' | 'models'

interface CommandItem {
  id: string
  group: CommandGroup
  label: string
  hint?: string
  icon: typeof MessageIcon
  run: () => void
}

const PAGE_ITEMS: Array<{ href: string; key: string; icon: typeof MessageIcon; admin?: boolean }> = [
  { href: '/sessions', key: 'chat', icon: MessageIcon },
  { href: '/archived', key: 'archived', icon: ArchiveIcon },
  { href: '/memory', key: 'memory', icon: SparkIcon },
  { href: '/integrations', key: 'integrations', icon: WrenchIcon },
  { href: '/media', key: 'media', icon: CloudIcon },
  { href: '/models', key: 'models', icon: CubeIcon },
  { href: '/usage', key: 'usage', icon: ChartIcon, admin: true },
  { href: '/audit', key: 'audit', icon: ShieldIcon, admin: true },
  { href: '/members', key: 'members', icon: UserIcon, admin: true },
  { href: '/settings', key: 'settings', icon: SettingsIcon },
]

function currentSessionId(pathname: string | null): string | null {
  const match = pathname ? /^\/chat\/([^/?#]+)/.exec(pathname) : null
  return match ? match[1] : null
}

export function CommandPalette({ isOpen, onClose }: { isOpen: boolean; onClose: () => void }) {
  const router = useRouter()
  const pathname = usePathname()
  const { user } = useAuth()
  const { t } = useI18n()
  const isAdmin = user?.role === 'owner' || user?.role === 'admin'
  const sessionId = currentSessionId(pathname)

  const [query, setQuery] = useState('')
  const [sessions, setSessions] = useState<ChatSession[]>([])
  const [models, setModels] = useState<TenantModel[]>([])
  const [activeIndex, setActiveIndex] = useState(0)
  const [creating, setCreating] = useState(false)
  const inputRef = useRef<HTMLInputElement | null>(null)
  const listRef = useRef<HTMLDivElement | null>(null)

  const load = useCallback(async () => {
    const [sessionResult, modelResult] = await Promise.allSettled([
      api.listSessions({ status: 'active', limit: 50 }),
      sessionId ? api.listTenantModels() : Promise.resolve(null),
    ])
    if (sessionResult.status === 'fulfilled') setSessions(sessionResult.value.sessions)
    if (modelResult.status === 'fulfilled' && modelResult.value) {
      setModels(modelResult.value.models)
    } else {
      setModels([])
    }
  }, [sessionId])

  useEffect(() => {
    if (!isOpen) return
    setQuery('')
    setActiveIndex(0)
    setCreating(false)
    void load()
    const frame = window.requestAnimationFrame(() => inputRef.current?.focus())
    return () => window.cancelAnimationFrame(frame)
  }, [isOpen, load])

  useEffect(() => {
    if (!isOpen) return
    const previous = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    return () => {
      document.body.style.overflow = previous
    }
  }, [isOpen])

  const items = useMemo<CommandItem[]>(() => {
    const keyword = query.trim().toLowerCase()
    const matches = (value: string) => !keyword || value.toLowerCase().includes(keyword)
    const next: CommandItem[] = []

    for (const page of PAGE_ITEMS) {
      if (page.admin && !isAdmin) continue
      const label = t(`nav.${page.key}`)
      if (!matches(label)) continue
      next.push({
        id: `page:${page.href}`,
        group: 'pages',
        label,
        icon: page.icon,
        run: () => {
          router.push(page.href)
          onClose()
        },
      })
    }

    if (matches(t('common.command.newSession'))) {
      next.push({
        id: 'action:new-session',
        group: 'pages',
        label: t('common.command.newSession'),
        icon: PlusIcon,
        run: () => {
          if (creating) return
          setCreating(true)
          void api
            .createSession({})
            .then(({ session }) => {
              router.push(`/chat/${session.id}`)
              onClose()
            })
            .catch((cause) => {
              toast.danger(ApiError.from(cause, t('chat.sessions.createFailed')).message)
            })
            .finally(() => setCreating(false))
        },
      })
    }

    for (const session of sessions) {
      const title = session.title?.trim() || t('chat.untitled')
      if (!matches(`${title} ${session.model ?? ''}`)) continue
      next.push({
        id: `session:${session.id}`,
        group: 'sessions',
        label: title,
        hint: session.model ?? undefined,
        icon: MessageIcon,
        run: () => {
          router.push(`/chat/${session.id}`)
          onClose()
        },
      })
    }

    if (sessionId) {
      for (const model of models) {
        const label = model.display_name || model.model
        if (!matches(`${label} ${model.model}`)) continue
        next.push({
          id: `model:${model.model}`,
          group: 'models',
          label,
          hint: model.model,
          icon: CubeIcon,
          run: () => {
            window.dispatchEvent(
              new CustomEvent(CHANGE_MODEL_EVENT, { detail: { model: model.model } }),
            )
            onClose()
          },
        })
      }
    }

    return next
  }, [creating, isAdmin, models, onClose, query, router, sessionId, sessions, t])

  useEffect(() => {
    setActiveIndex((index) => Math.min(index, Math.max(0, items.length - 1)))
  }, [items.length])

  useEffect(() => {
    if (!isOpen) return
    const node = listRef.current?.querySelector<HTMLElement>(`[data-command-index="${activeIndex}"]`)
    node?.scrollIntoView({ block: 'nearest' })
  }, [activeIndex, isOpen])

  useEffect(() => {
    if (!isOpen) return
    onClose()
    // 路由变化后关闭面板；onClose 不参与依赖，避免父级重建函数导致循环
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pathname])

  function handleKeyDown(event: KeyboardEvent<HTMLInputElement>) {
    if (event.key === 'Escape') {
      event.preventDefault()
      onClose()
      return
    }
    if (event.key === 'ArrowDown') {
      event.preventDefault()
      if (items.length === 0) return
      setActiveIndex((index) => (index + 1) % items.length)
      return
    }
    if (event.key === 'ArrowUp') {
      event.preventDefault()
      if (items.length === 0) return
      setActiveIndex((index) => (index - 1 + items.length) % items.length)
      return
    }
    if (event.key === 'Enter') {
      event.preventDefault()
      items[activeIndex]?.run()
    }
  }

  if (!isOpen) return null

  let renderIndex = -1
  const groups: Array<{ key: CommandGroup; label: string }> = [
    { key: 'pages', label: t('common.command.pages') },
    { key: 'sessions', label: t('common.command.sessions') },
    { key: 'models', label: t('common.command.models') },
  ]

  return (
    <div
      className="fixed inset-0 z-[80] flex items-start justify-center bg-background/50 px-3 pt-[10vh] backdrop-blur-sm"
      onClick={onClose}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={t('common.command.title')}
        onClick={(event) => event.stopPropagation()}
        className="flex w-full max-w-xl flex-col overflow-hidden rounded-xl border border-border bg-surface shadow-2xl"
      >
        <input
          ref={inputRef}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={handleKeyDown}
          role="combobox"
          aria-expanded="true"
          aria-controls="command-list"
          aria-activedescendant={items[activeIndex] ? `command-${items[activeIndex].id}` : undefined}
          placeholder={t('common.command.placeholder')}
          className="h-12 w-full border-b border-border bg-transparent px-4 text-sm text-foreground outline-none placeholder:text-subtle"
        />

        <div
          id="command-list"
          ref={listRef}
          role="listbox"
          aria-label={t('common.command.title')}
          className="max-h-[55vh] overflow-y-auto py-1"
        >
          {items.length === 0 ? (
            <p className="px-4 py-10 text-center text-sm text-muted">{t('common.command.empty')}</p>
          ) : (
            groups.map((group) => {
              const groupItems = items.filter((item) => item.group === group.key)
              if (groupItems.length === 0) return null
              return (
                <div key={group.key} className="py-1">
                  <p className="px-4 py-1 text-[10.5px] font-medium uppercase tracking-wider text-muted">
                    {group.label}
                  </p>
                  {groupItems.map((item) => {
                    renderIndex += 1
                    const index = renderIndex
                    const active = index === activeIndex
                    const Icon = item.icon
                    return (
                      <button
                        key={item.id}
                        id={`command-${item.id}`}
                        data-command-index={index}
                        type="button"
                        role="option"
                        aria-selected={active}
                        onMouseEnter={() => setActiveIndex(index)}
                        onClick={item.run}
                        className={cn(
                          'flex min-h-10 w-full items-center gap-2.5 px-4 py-2 text-left text-[13px] transition-colors',
                          active ? 'bg-accent-soft text-foreground' : 'text-foreground/85',
                        )}
                      >
                        <Icon size={15} className="shrink-0 text-muted" />
                        <span className="min-w-0 flex-1 truncate">{item.label}</span>
                        {item.hint ? (
                          <span className="max-w-[45%] shrink-0 truncate font-mono text-[10.5px] text-subtle">
                            {item.hint}
                          </span>
                        ) : null}
                      </button>
                    )
                  })}
                </div>
              )
            })
          )}
        </div>

        <div className="flex items-center justify-between gap-2 border-t border-border px-4 py-2 text-[10.5px] text-subtle">
          <span>{creating ? t('common.command.creating') : t('common.command.hint')}</span>
          <button
            type="button"
            onClick={onClose}
            className="rounded border border-border px-1.5 py-0.5 font-mono text-[10px] text-muted hover:text-foreground"
          >
            Esc
          </button>
        </div>
      </div>
    </div>
  )
}

export default CommandPalette
