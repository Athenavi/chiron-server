'use client'

import { useRef, type ReactNode } from 'react'
import { TOGGLE_SIDEBAR_EVENT } from '@/components/chat/ChatHeader'
import { MenuIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'

/**
 * 管理类页面通用页头：移动端菜单按钮（触发抽屉）+ 标题/描述 + 右侧操作区。
 * 桌面端布局已有固定侧栏，菜单按钮隐藏。
 */
export function PageHeader({
  title,
  description,
  actions,
  children,
}: {
  title: string
  description?: string
  actions?: ReactNode
  children?: ReactNode
}) {
  const { t } = useI18n()
  /** 触发抽屉的按钮：放进事件 detail，壳层关闭抽屉后把焦点还给它。 */
  const menuButtonRef = useRef<HTMLButtonElement | null>(null)

  function openSidebar() {
    window.dispatchEvent(
      new CustomEvent(TOGGLE_SIDEBAR_EVENT, { detail: { trigger: menuButtonRef.current } }),
    )
  }

  return (
    /*
     * 使用 PageHeader 的页面在壳层里不渲染 48px 移动顶栏（菜单由这里提供），
     * 因此页头始终贴 `top-0`；桌面端同样无壳层顶栏。
     */
    <header className="sticky top-0 z-20 border-b border-border bg-background/90 backdrop-blur">
      <div className="mx-auto flex w-full max-w-5xl flex-col gap-3 px-4 py-3 md:px-6">
        <div className="flex items-center gap-2">
          <button
            ref={menuButtonRef}
            type="button"
            aria-label={t('common.openSidebar')}
            onClick={openSidebar}
            className="inline-flex h-10 w-10 items-center justify-center rounded-md text-muted transition-colors hover:bg-default hover:text-foreground md:hidden"
          >
            <MenuIcon size={16} aria-hidden="true" />
          </button>
          <div className="min-w-0 flex-1">
            <h1 className="truncate text-[15px] font-semibold tracking-tight">{title}</h1>
            {description ? <p className="mt-0.5 text-xs text-muted">{description}</p> : null}
          </div>
          {actions ? <div className="flex shrink-0 items-center gap-2">{actions}</div> : null}
        </div>
        {children}
      </div>
    </header>
  )
}
