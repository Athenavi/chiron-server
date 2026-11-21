'use client'

/**
 * 前台应用壳：深色会话侧栏（桌面常驻 280px，可折叠为 72px 图标 rail / 移动端 HeroUI Drawer）
 * + 浅色主区。
 *
 * - 认证守卫保持原方式：`useAuth()` + 未登录跳 `/login`、必须改密跳 `/change-password`；
 * - 侧栏内容（品牌、会话列表、用户区）统一由 `SessionSidebar` 承担；
 * - 主区顶部不再重复做 header：聊天页由 `ChatHeader` 自己负责，
 *   其余页面（会话列表 / 设置）在移动端用这里提供的极简顶栏作为菜单入口；
 * - 移动端抽屉通过 `window` 事件 `chiron:toggle-sidebar`（常量 `TOGGLE_SIDEBAR_EVENT`）
 *   由 `ChatHeader` 的菜单按钮触发，路由变化时自动关闭；
 * - 桌面折叠：状态写到 `<html data-sidebar>`（首屏脚本 + React 共同维护），
 *   CSS 变量 `--app-sidebar-width` 同时驱动侧栏宽度与主区左边距，
 *   所以整页刷新时首帧就是折叠后的宽度，不会闪一下 280px。
 */

import { useEffect, useRef, useState } from 'react'
import Link from 'next/link'
import { usePathname, useRouter } from 'next/navigation'
import { Drawer } from '@heroui/react'
import { useAuth } from '@/components/auth/AuthProvider'
import { useI18n } from '@/lib/i18n'
import { TOGGLE_SIDEBAR_EVENT } from '@/components/chat/ChatHeader'
import { SessionSidebar } from '@/components/sidebar/SessionSidebar'
import { WorkspaceNav } from '@/components/shell/WorkspaceNav'
import { CommandPalette } from '@/components/shell/CommandPalette'
import {
  ChevronLeftIcon,
  ChevronRightIcon,
  ChironMark,
  MenuIcon,
  XIcon,
} from '@/components/ui-bridge/icons'
import { Skeleton } from '@/components/ui-bridge/skeleton'
import { cn } from '@/lib/utils'

/** 折叠偏好持久化键（内联脚本、React 与下面的常量必须一致）。 */
const SIDEBAR_STORAGE_KEY = 'chiron.sidebar.collapsed'

/**
 * 首屏脚本：在侧栏 DOM 之前执行，读 localStorage 写 `<html data-sidebar>`。
 *
 * 折叠与否完全由这个属性驱动（`app/globals.css` 的 `--app-sidebar-width`），
 * 因此脚本一跑完，侧栏宽度与主区左边距在首帧就是对的。
 * try/catch 兜底隐私模式下 localStorage 抛异常的情况。
 */
const SIDEBAR_INIT_SCRIPT = `(function () {
  try {
    var collapsed = window.localStorage.getItem('${SIDEBAR_STORAGE_KEY}') === '1';
    document.documentElement.setAttribute('data-sidebar', collapsed ? 'collapsed' : 'expanded');
  } catch (error) {
    document.documentElement.setAttribute('data-sidebar', 'expanded');
  }
})();`

/** 读取已持久化的折叠偏好（localStorage 不可用时按展开处理）。 */
function readCollapsedPreference(): boolean {
  try {
    return window.localStorage.getItem(SIDEBAR_STORAGE_KEY) === '1'
  } catch {
    return false
  }
}

/** 写入偏好并同步 `<html data-sidebar>`；CSS 与主区留白都跟着它走。 */
function applyCollapsedPreference(collapsed: boolean) {
  document.documentElement.setAttribute('data-sidebar', collapsed ? 'collapsed' : 'expanded')
  try {
    window.localStorage.setItem(SIDEBAR_STORAGE_KEY, collapsed ? '1' : '0')
  } catch {
    /* 隐私模式：只在本次会话生效 */
  }
}

/** 从 `/chat/{id}` 里取当前会话 id，供侧栏高亮。 */
function activeSessionIdFromPath(pathname: string | null): string | undefined {
  const match = pathname ? /^\/chat\/([^/?#]+)/.exec(pathname) : null
  return match ? match[1] : undefined
}

interface SidebarShellProps {
  activeSessionId?: string
  /** 点击品牌 / 会话后关闭移动端抽屉（桌面端是 no-op）。 */
  onNavigate?: () => void
  /** 移动端抽屉右上角的关闭按钮，桌面端不渲染。 */
  onClose?: () => void
  /** 桌面是否处于折叠（图标 rail）状态；抽屉不传，恒为展开。 */
  collapsed?: boolean
  /** 桌面折叠按钮；不传则不渲染按钮（移动端抽屉）。 */
  onToggleCollapse?: () => void
}

function SidebarShell({
  activeSessionId,
  onNavigate,
  onClose,
  collapsed = false,
  onToggleCollapse,
}: SidebarShellProps) {
  const { t } = useI18n()
  const collapseLabel = collapsed ? t('chat.sidebar.expand') : t('chat.sidebar.collapse')
  return (
    <div className="flex h-full min-h-0 flex-col bg-sidebar text-sidebar-foreground">
      <div className="sidebar-header flex h-14 shrink-0 items-center justify-between gap-1 px-4">
        <Link
          href="/sessions"
          onClick={onNavigate}
          aria-label="Chiron"
          className="flex items-center gap-2 rounded-md text-[15px] font-semibold tracking-tight focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring"
        >
          <ChironMark size={20} className="text-sidebar-foreground" />
          {/* 折叠时由 CSS 隐藏字标，只留品牌 mark */}
          <span className="sidebar-expanded-only">Chiron</span>
        </Link>
        {onToggleCollapse ? (
          <button
            type="button"
            onClick={onToggleCollapse}
            aria-controls="app-sidebar"
            aria-expanded={!collapsed}
            aria-label={collapseLabel}
            title={collapseLabel}
            className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-sidebar-muted transition-colors hover:bg-sidebar-hover hover:text-sidebar-foreground"
          >
            {/* 箭头也交给 CSS 二选一，避免首帧方向与宽度不一致 */}
            <ChevronLeftIcon size={16} className="sidebar-chevron-expanded" />
            <ChevronRightIcon size={16} className="sidebar-chevron-collapsed" />
          </button>
        ) : null}
        {onClose ? (
          <button
            type="button"
            onClick={onClose}
            aria-label={t('common.closeSidebar')}
            className="inline-flex h-10 w-10 items-center justify-center rounded-md text-sidebar-muted transition-colors hover:bg-sidebar-hover hover:text-sidebar-foreground md:h-9 md:w-9"
          >
            <XIcon size={16} />
          </button>
        ) : null}
      </div>

      <WorkspaceNav onNavigate={onNavigate} collapsed={collapsed} />
      <SessionSidebar activeSessionId={activeSessionId} />
    </div>
  )
}

export default function AppLayout({ children }: { children: React.ReactNode }) {
  const { user, loading, authenticated } = useAuth()
  const { t } = useI18n()
  const router = useRouter()
  const pathname = usePathname()
  const [drawerOpen, setDrawerOpen] = useState(false)
  const [commandOpen, setCommandOpen] = useState(false)
  /**
   * 桌面侧栏折叠状态。首帧渲染按「展开」输出（和 SSR 一致，不产生 hydration 差异），
   * 真正的视觉宽度由 `<html data-sidebar>` + CSS 变量决定，所以不会闪；
   * 这个 state 只服务于折叠按钮的 `aria-expanded` / `aria-label` 与导航项的原生 title。
   */
  const [collapsed, setCollapsed] = useState(false)
  /** 打开抽屉的按钮；关闭后把焦点还回去（HeroUI 不知道是谁触发的）。 */
  const drawerTriggerRef = useRef<HTMLElement | null>(null)
  const mobileMenuRef = useRef<HTMLButtonElement | null>(null)

  const activeSessionId = activeSessionIdFromPath(pathname)
  const isChatRoute = Boolean(pathname?.startsWith('/chat'))
  // 这些页面自带 PageHeader（含移动端菜单），壳层不再渲染第二条 48px 顶栏。
  const hasPageHeader = [
    '/memory',
    '/integrations',
    '/media',
    '/models',
    '/archived',
    '/usage',
    '/audit',
    '/members',
  ].some((route) => pathname?.startsWith(route))

  // 未登录 → /login；必须先改密 → /change-password
  useEffect(() => {
    if (loading) return
    if (!authenticated) {
      const next = pathname && pathname !== '/' ? `?next=${encodeURIComponent(pathname)}` : ''
      router.replace(`/login${next}`)
      return
    }
    if (user?.must_change_password) router.replace('/change-password')
  }, [loading, authenticated, user, pathname, router])

  // 路由切换关闭移动端抽屉（点会话跳转后自动收起）
  useEffect(() => {
    setDrawerOpen(false)
  }, [pathname])

  // Cmd/Ctrl+K 打开命令面板；再按一次关闭。
  useEffect(() => {
    const handler = (event: KeyboardEvent) => {
      if (!(event.metaKey || event.ctrlKey) || event.key.toLowerCase() !== 'k') return
      event.preventDefault()
      setCommandOpen((open) => !open)
    }
    window.addEventListener('keydown', handler)
    return () => window.removeEventListener('keydown', handler)
  }, [])

  // 首屏脚本已经把偏好写进 `<html data-sidebar>`，这里把它同步进 React 状态
  useEffect(() => {
    setCollapsed(readCollapsedPreference())
  }, [])

  function toggleCollapsed() {
    const next = !collapsed
    setCollapsed(next)
    applyCollapsedPreference(next)
  }

  // ChatHeader 的菜单按钮通过 window 事件开合抽屉（事件 detail 带上触发元素，便于关闭后回焦）
  useEffect(() => {
    const handler = (event: Event) => {
      const detail = (event as CustomEvent<{ trigger?: HTMLElement | null }>).detail
      if (detail?.trigger instanceof HTMLElement) drawerTriggerRef.current = detail.trigger
      setDrawerOpen((previous) => !previous)
    }
    window.addEventListener(TOGGLE_SIDEBAR_EVENT, handler)
    return () => window.removeEventListener(TOGGLE_SIDEBAR_EVENT, handler)
  }, [])

  function handleDrawerOpenChange(open: boolean) {
    setDrawerOpen(open)
    if (open) return
    const trigger = drawerTriggerRef.current
    if (!trigger) return
    window.requestAnimationFrame(() => {
      if (trigger.isConnected && !trigger.hasAttribute('disabled')) trigger.focus()
    })
  }

  if (loading || !authenticated || user?.must_change_password) {
    return (
      <div className="flex min-h-dvh items-center justify-center bg-background">
        {/* 认证期间也先跑一次侧栏脚本，保证侧栏一出现就是折叠后的宽度 */}
        <script dangerouslySetInnerHTML={{ __html: SIDEBAR_INIT_SCRIPT }} />
        <div
          className="flex w-56 flex-col items-center gap-3"
          role="status"
          aria-live="polite"
          aria-busy="true"
        >
          <ChironMark size={28} className="text-primary" />
          <Skeleton className="h-2.5 w-32" />
          <span className="sr-only">{t('auth.checkingSession')}</span>
        </div>
      </div>
    )
  }

  return (
    <div className="min-h-dvh bg-background">
      {/* 首屏侧栏脚本：必须在侧栏 DOM 之前执行，整页刷新时首帧即是折叠后的宽度 */}
      <script dangerouslySetInnerHTML={{ __html: SIDEBAR_INIT_SCRIPT }} />

      <a
        href="#main-content"
        className="sr-only focus:not-sr-only focus:fixed focus:left-3 focus:top-3 focus:z-[70] focus:rounded-md focus:bg-surface focus:px-3 focus:py-2 focus:text-sm focus:text-foreground focus:shadow-md"
      >
        {t('common.skipToContent')}
      </a>

      {/* 桌面：固定深色侧栏（宽度由 --app-sidebar-width 驱动：280px / 折叠 72px） */}
      <aside
        id="app-sidebar"
        aria-label={t('nav.sidebarLabel')}
        className={cn(
          'fixed inset-y-0 left-0 z-40 hidden flex-col border-r border-sidebar-border bg-sidebar',
          'text-sidebar-foreground md:flex',
        )}
      >
        <SidebarShell
          activeSessionId={activeSessionId}
          collapsed={collapsed}
          onToggleCollapse={toggleCollapsed}
        />
      </aside>

      {/* 移动端：HeroUI Drawer（关闭时 React Aria 不渲染面板，不会重复拉取会话列表） */}
      <Drawer isOpen={drawerOpen} onOpenChange={handleDrawerOpenChange}>
        <Drawer.Backdrop variant="blur">
          <Drawer.Content placement="left">
            <Drawer.Dialog
              aria-label={t('nav.sidebarLabel')}
              className="w-[280px] max-w-[calc(100vw_-_3.5rem)] bg-sidebar p-0 text-sidebar-foreground"
            >
              <SidebarShell
                activeSessionId={activeSessionId}
                onNavigate={() => setDrawerOpen(false)}
                onClose={() => setDrawerOpen(false)}
              />
            </Drawer.Dialog>
          </Drawer.Content>
        </Drawer.Backdrop>
      </Drawer>

      <CommandPalette isOpen={commandOpen} onClose={() => setCommandOpen(false)} />

      {/* 折叠时左边距 280 → 72 走 CSS 变量，切换只做一次 margin 过渡 */}
      <div className="flex min-h-dvh flex-col transition-[margin] duration-200 md:ml-[var(--app-sidebar-width)]">
        {/* 移动端菜单入口：聊天页由 ChatHeader、管理页由 PageHeader 自带，避免双顶栏 */}
        {isChatRoute || hasPageHeader ? null : (
          <header className="sticky top-0 z-30 flex h-12 items-center gap-2 border-b border-border bg-background/85 px-2 backdrop-blur md:hidden">
            <button
              ref={mobileMenuRef}
              type="button"
              onClick={() => {
                drawerTriggerRef.current = mobileMenuRef.current
                setDrawerOpen(true)
              }}
              aria-label={t('common.openSidebar')}
              aria-expanded={drawerOpen}
              className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted transition-colors hover:bg-surface-hover hover:text-foreground"
            >
              <MenuIcon size={18} />
            </button>
            <span className="text-sm font-semibold">Chiron</span>
          </header>
        )}

        {/* tabIndex=-1 让跳过链接能把焦点真正移到主内容 */}
        <main id="main-content" tabIndex={-1} className="min-w-0 flex-1 focus:outline-none">
          {children}
        </main>
      </div>
    </div>
  )
}
