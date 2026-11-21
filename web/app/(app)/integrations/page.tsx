'use client'

/**
 * 集成页：MCP / 插件 / 技能 / 市场四个面板，用 HeroUI Tabs 一页切换。
 *
 * - 页头提供标题、说明与「刷新」；刷新只递增 refreshToken，由各面板自行重拉列表；
 * - 每个面板独立加载 / 空态 / 错误 + 重试，写操作各自 isPending 防重；
 * - HeroUI Tabs 默认只挂载当前面板，切走再切回会重新拉取，保证数据新鲜。
 */

import { useState } from 'react'
import { Button, Tabs } from '@heroui/react'
import { PageHeader } from '@/components/shell/PageHeader'
import { RefreshIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import { McpPanel } from '@/components/integrations/McpPanel'
import { PluginsPanel } from '@/components/integrations/PluginsPanel'
import { SkillsPanel } from '@/components/integrations/SkillsPanel'
import { MarketPanel } from '@/components/integrations/MarketPanel'

/** label 存 i18n 键，渲染时再翻译。 */
const TABS: Array<{ id: string; labelKey: string }> = [
  { id: 'mcp', labelKey: 'integrations.page.tabs.mcp' },
  { id: 'plugins', labelKey: 'integrations.page.tabs.plugins' },
  { id: 'skills', labelKey: 'integrations.page.tabs.skills' },
  { id: 'market', labelKey: 'integrations.page.tabs.market' },
]

type TabKey = 'mcp' | 'plugins' | 'skills' | 'market'

export default function IntegrationsPage() {
  const { t } = useI18n()
  const [tab, setTab] = useState<TabKey>('mcp')
  const [refreshToken, setRefreshToken] = useState(0)

  return (
    <div>
      <PageHeader
        title={t('integrations.page.title')}
        description={t('integrations.page.description')}
        actions={
          <Button
            variant="secondary"
            size="sm"
            className="h-10 gap-1.5 md:h-9"
            onPress={() => setRefreshToken((value) => value + 1)}
          >
            <RefreshIcon size={14} aria-hidden="true" />
            {t('integrations.page.refresh')}
          </Button>
        }
      />

      {/* 与 PageHeader 的 max-w-5xl 对齐，避免标签条比页头更宽 */}
      <div className="content-shell max-w-5xl py-6 md:py-8">
        <Tabs
          selectedKey={tab}
          onSelectionChange={(key) => {
            if (key !== null) setTab(String(key) as TabKey)
          }}
          variant="secondary"
        >
          {/* 窄屏标签条可横向滚动，避免四个 Tab 撑破布局 */}
          <Tabs.ListContainer className="max-w-full overflow-x-auto">
            <Tabs.List aria-label={t('integrations.page.tabs.label')}>
              {TABS.map((item) => (
                <Tabs.Tab key={item.id} id={item.id} className="h-10 md:h-8">
                  {t(item.labelKey)}
                </Tabs.Tab>
              ))}
            </Tabs.List>
          </Tabs.ListContainer>

          <Tabs.Panel id="mcp" className="pt-5">
            <McpPanel refreshToken={refreshToken} />
          </Tabs.Panel>
          <Tabs.Panel id="plugins" className="pt-5">
            <PluginsPanel refreshToken={refreshToken} />
          </Tabs.Panel>
          <Tabs.Panel id="skills" className="pt-5">
            <SkillsPanel refreshToken={refreshToken} />
          </Tabs.Panel>
          <Tabs.Panel id="market" className="pt-5">
            <MarketPanel refreshToken={refreshToken} />
          </Tabs.Panel>
        </Tabs>
      </div>
    </div>
  )
}
