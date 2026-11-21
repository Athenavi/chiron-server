'use client'

import Link from 'next/link'
import { usePathname } from 'next/navigation'
import { useAuth } from '@/components/auth/AuthProvider'
import { useI18n } from '@/lib/i18n'
import {
  ChartIcon,
  CloudIcon,
  CubeIcon,
  MessageIcon,
  ShieldIcon,
  SparkIcon,
  UserIcon,
  WrenchIcon,
} from '@/components/ui-bridge/icons'
import { cn } from '@/lib/utils'

interface NavItem {
  href: string
  key: string
  icon: typeof MessageIcon
  match: (path: string) => boolean
}

const MAIN_ITEMS: NavItem[] = [
  {
    href: '/sessions',
    key: 'chat',
    icon: MessageIcon,
    match: (p) => p === '/sessions' || p.startsWith('/chat'),
  },
  { href: '/memory', key: 'memory', icon: SparkIcon, match: (p) => p.startsWith('/memory') },
  {
    href: '/integrations',
    key: 'integrations',
    icon: WrenchIcon,
    match: (p) => p.startsWith('/integrations'),
  },
  { href: '/media', key: 'media', icon: CloudIcon, match: (p) => p.startsWith('/media') },
  { href: '/models', key: 'models', icon: CubeIcon, match: (p) => p.startsWith('/models') },
]

const ADMIN_ITEMS: NavItem[] = [
  { href: '/usage', key: 'usage', icon: ChartIcon, match: (p) => p.startsWith('/usage') },
  { href: '/audit', key: 'audit', icon: ShieldIcon, match: (p) => p.startsWith('/audit') },
  { href: '/members', key: 'members', icon: UserIcon, match: (p) => p.startsWith('/members') },
]

function NavLink({
  item,
  pathname,
  onNavigate,
  collapsed,
}: {
  item: NavItem
  pathname: string
  onNavigate?: () => void
  collapsed?: boolean
}) {
  const { t } = useI18n()
  const active = item.match(pathname)
  const Icon = item.icon
  const label = t(`nav.${item.key}`)
  return (
    <li>
      <Link
        href={item.href}
        onClick={onNavigate}
        aria-current={active ? 'page' : undefined}
        // 折叠成图标 rail 后补一个原生 tooltip；展开时文字本身就是提示
        title={collapsed ? label : undefined}
        className={cn(
          'sidebar-nav-link flex h-9 items-center gap-2.5 rounded-md px-2.5 text-[13px] transition-colors',
          'focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring',
          active
            ? 'bg-sidebar-active text-sidebar-foreground'
            : 'text-sidebar-muted hover:bg-sidebar-hover hover:text-sidebar-foreground',
        )}
      >
        <Icon size={16} aria-hidden="true" />
        {/* rail 里只做视觉隐藏，可访问名称仍然保留 */}
        <span className="sidebar-label">{label}</span>
      </Link>
    </li>
  )
}

export function WorkspaceNav({
  onNavigate,
  collapsed,
}: {
  onNavigate?: () => void
  /** 桌面侧栏折叠为图标 rail（抽屉不传）。 */
  collapsed?: boolean
}) {
  const pathname = usePathname() ?? ''
  const { user } = useAuth()
  const { t } = useI18n()
  const isAdmin = user?.role === 'owner' || user?.role === 'admin'

  return (
    <nav aria-label={t('nav.label')} className="sidebar-nav shrink-0 px-2 pb-2">
      <ul className="flex flex-col gap-0.5">
        {MAIN_ITEMS.map((item) => (
          <NavLink
            key={item.href}
            item={item}
            pathname={pathname}
            onNavigate={onNavigate}
            collapsed={collapsed}
          />
        ))}
      </ul>
      {isAdmin ? (
        <>
          {/* rail 里没有分组标题，用一条短分隔线代替 */}
          <div
            aria-hidden="true"
            className="sidebar-rail-only mx-auto my-1.5 h-px w-8 bg-sidebar-border"
          />
          <p className="sidebar-expanded-only px-2.5 pb-1 pt-3 text-[10px] font-medium uppercase tracking-wider text-sidebar-muted">
            {t('nav.admin')}
          </p>
          <ul className="flex flex-col gap-0.5">
            {ADMIN_ITEMS.map((item) => (
              <NavLink
                key={item.href}
                item={item}
                pathname={pathname}
                onNavigate={onNavigate}
                collapsed={collapsed}
              />
            ))}
          </ul>
        </>
      ) : null}
    </nav>
  )
}
