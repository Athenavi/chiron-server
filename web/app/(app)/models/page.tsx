'use client'

/**
 * 模型页。
 *
 * - 默认是用户视图：租户启用的模型卡片 + 「在选择器中隐藏」Switch（服务端保存）；
 * - 管理员在页头能看到「管理模型服务」入口，进入后是三个标签页
 *   （模型 / Provider 与密钥 / 路由），每个标签页独立加载、空态、错误重试；
 * - 管理入口只对 owner / admin 渲染，普通成员不会看到；
 * - 文案全部走 `models.*` i18n 命名空间。
 */

import { useEffect, useState } from 'react'
import { Button, Tabs } from '@heroui/react'
import { PageHeader } from '@/components/shell/PageHeader'
import { UserModelsPanel } from '@/components/models/UserModelsPanel'
import { AdminCatalogPanel } from '@/components/models/AdminCatalogPanel'
import { AdminProvidersPanel } from '@/components/models/AdminProvidersPanel'
import { AdminRoutesPanel } from '@/components/models/AdminRoutesPanel'
import { ChevronLeftIcon, RefreshIcon, SettingsIcon } from '@/components/ui-bridge/icons'
import { useAuth } from '@/components/auth/AuthProvider'
import { useI18n } from '@/lib/i18n'

type TabKey = 'models' | 'providers' | 'routes'

/** label 存 i18n 键，渲染时再翻译。 */
const TABS: Array<{ id: TabKey; labelKey: string }> = [
  { id: 'models', labelKey: 'models.admin.tabs.models' },
  { id: 'providers', labelKey: 'models.admin.tabs.providers' },
  { id: 'routes', labelKey: 'models.admin.tabs.routes' },
]

export default function ModelsPage() {
  const { t } = useI18n()
  const { user } = useAuth()
  const isAdmin = user?.role === 'owner' || user?.role === 'admin'

  const [mode, setMode] = useState<'list' | 'manage'>('list')
  const [tab, setTab] = useState<TabKey>('models')
  const [refreshToken, setRefreshToken] = useState(0)

  // 登录态晚到 / 角色被降级时，别让非管理员停在管理视图。
  useEffect(() => {
    if (!isAdmin) setMode('list')
  }, [isAdmin])

  return (
    <div>
      <PageHeader
        title={t('models.title')}
        description={t('models.description')}
        actions={
          <>
            <Button
              variant="secondary"
              size="sm"
              className="h-10 gap-1.5 md:h-9"
              onPress={() => setRefreshToken((value) => value + 1)}
            >
              <RefreshIcon size={14} aria-hidden="true" />
              {t('common.refresh')}
            </Button>
            {isAdmin && mode === 'list' ? (
              <Button
                variant="primary"
                size="sm"
                className="h-10 gap-1.5 md:h-9"
                onPress={() => setMode('manage')}
              >
                <SettingsIcon size={14} aria-hidden="true" />
                {t('models.view.manage')}
              </Button>
            ) : null}
            {isAdmin && mode === 'manage' ? (
              <Button
                variant="secondary"
                size="sm"
                className="h-10 gap-1.5 md:h-9"
                onPress={() => setMode('list')}
              >
                <ChevronLeftIcon size={14} aria-hidden="true" />
                {t('models.view.back')}
              </Button>
            ) : null}
          </>
        }
      />

      <div className="content-shell max-w-5xl py-6 md:py-8">
        {mode === 'manage' ? (
          <Tabs
            selectedKey={tab}
            onSelectionChange={(key) => {
              if (key !== null) setTab(String(key) as TabKey)
            }}
            variant="secondary"
          >
            {/* 窄屏标签条可横向滚动；不使用 Tabs.Indicator（已知会崩）。 */}
            <Tabs.ListContainer className="max-w-full overflow-x-auto">
              <Tabs.List aria-label={t('models.admin.tabs.label')}>
                {TABS.map((item) => (
                  <Tabs.Tab key={item.id} id={item.id} className="h-10 md:h-8">
                    {t(item.labelKey)}
                  </Tabs.Tab>
                ))}
              </Tabs.List>
            </Tabs.ListContainer>

            <Tabs.Panel id="models" className="pt-5">
              <AdminCatalogPanel refreshToken={refreshToken} />
            </Tabs.Panel>
            <Tabs.Panel id="providers" className="pt-5">
              <AdminProvidersPanel
                refreshToken={refreshToken}
                // 发现落库 / 健康探测会改动目录与路由健康，顺带刷新其他面板。
                onDataChanged={() => setRefreshToken((value) => value + 1)}
              />
            </Tabs.Panel>
            <Tabs.Panel id="routes" className="pt-5">
              <AdminRoutesPanel refreshToken={refreshToken} />
            </Tabs.Panel>
          </Tabs>
        ) : (
          <UserModelsPanel refreshToken={refreshToken} />
        )}
      </div>
    </div>
  )
}
