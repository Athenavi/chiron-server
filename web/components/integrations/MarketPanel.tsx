'use client'

/**
 * 市场面板。
 *
 * - kind 过滤（全部 / plugin / skill）；
 * - 条目卡：name、kind、version、source、signed 徽标、description、published_at；
 * - 安装 / 卸载：默认 user 范围，owner / admin 可切到 tenant 范围；
 * - 成功 toast 并刷新列表，失败展示后端 message。
 *
 * 文案全部走 i18n；kind 徽标与 signed 徽标由词典映射，未知 kind 回落原始值。
 */

import { useCallback, useEffect, useState } from 'react'
import {
  Button,
  Card,
  Chip,
  Label,
  ListBox,
  Select,
  Switch,
  toast,
} from '@heroui/react'
import { useAuth } from '@/components/auth/AuthProvider'
import { api } from '@/lib/api'
import { ApiError, type MarketItem, type MarketItemKind, type MarketScope } from '@/lib/types'
import { formatDateTime } from '@/lib/utils'
import { useI18n } from '@/lib/i18n'
import {
  CheckCircleIcon,
  PlusIcon,
  SparkIcon,
  TrashIcon,
} from '@/components/ui-bridge/icons'
import { ConfirmDialog, ListSkeleton, PanelEmpty, PanelError } from './shared'

type KindFilter = 'all' | MarketItemKind

const KIND_OPTIONS: Array<{ value: KindFilter; labelKey: string }> = [
  { value: 'all', labelKey: 'integrations.market.kindAll' },
  { value: 'plugin', labelKey: 'integrations.market.kindPlugin' },
  { value: 'skill', labelKey: 'integrations.market.kindSkill' },
]

/** kind → 词典键；未知类型回落到原始 kind 字符串。 */
const KIND_LABEL_KEYS: Record<string, string> = {
  plugin: 'integrations.market.kindPluginBadge',
  skill: 'integrations.market.kindSkillBadge',
}

export interface MarketPanelProps {
  /** 页面「刷新」按钮递增；面板据此重新拉取列表。 */
  refreshToken: number
}

export function MarketPanel({ refreshToken }: MarketPanelProps) {
  const { t, locale } = useI18n()
  const { user } = useAuth()
  const canTenant = user?.role === 'owner' || user?.role === 'admin'

  const [kind, setKind] = useState<KindFilter>('all')
  const [tenantScope, setTenantScope] = useState(false)
  const [items, setItems] = useState<MarketItem[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const [installingId, setInstallingId] = useState<string | null>(null)
  const [uninstalling, setUninstalling] = useState(false)
  const [uninstallError, setUninstallError] = useState<string | null>(null)
  const [pendingUninstall, setPendingUninstall] = useState<MarketItem | null>(null)

  const scope: MarketScope = canTenant && tenantScope ? 'tenant' : 'user'
  const scopeLabel =
    scope === 'tenant' ? t('integrations.market.scopeTenant') : t('integrations.market.scopeUser')

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      setItems(await api.listMarketItems(kind === 'all' ? {} : { kind }))
    } catch (cause) {
      setError(ApiError.from(cause, t('integrations.market.loadError')).message)
    } finally {
      setLoading(false)
    }
  }, [kind, t])

  useEffect(() => {
    void load()
  }, [load, refreshToken])

  async function handleInstall(item: MarketItem) {
    if (installingId) return
    setInstallingId(item.id)
    try {
      await api.installMarketItem(item.item_id, scope)
      toast.success(t('integrations.market.installSuccess', { name: item.name, scope: scopeLabel }))
      await load()
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('integrations.market.installError')).message)
    } finally {
      setInstallingId(null)
    }
  }

  async function handleUninstall() {
    const target = pendingUninstall
    if (!target || uninstalling) return
    setUninstalling(true)
    setUninstallError(null)
    try {
      const result = await api.uninstallMarketItem(target.item_id, scope)
      setPendingUninstall(null)
      if (result.removed > 0) {
        toast.success(
          t('integrations.market.uninstallSuccess', { name: target.name, scope: scopeLabel }),
        )
      } else {
        toast.info(t('integrations.market.uninstallNone', { name: target.name, scope: scopeLabel }))
      }
      await load()
    } catch (cause) {
      setUninstallError(ApiError.from(cause, t('integrations.market.uninstallError')).message)
    } finally {
      setUninstalling(false)
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <p className="max-w-prose text-[13px] leading-6 text-muted">
          {t('integrations.market.intro')}
        </p>

        <div className="flex flex-wrap items-end gap-3">
          <Select
            selectedKey={kind}
            isDisabled={loading}
            onSelectionChange={(key) => {
              if (key !== null) setKind(String(key) as KindFilter)
            }}
            className="w-40"
          >
            <Label>{t('integrations.market.kindLabel')}</Label>
            <Select.Trigger>
              <Select.Value />
              <Select.Indicator />
            </Select.Trigger>
            <Select.Popover>
              <ListBox>
                {KIND_OPTIONS.map((option) => (
                  <ListBox.Item
                    key={option.value}
                    id={option.value}
                    textValue={t(option.labelKey)}
                  >
                    {t(option.labelKey)}
                    <ListBox.Item.Indicator />
                  </ListBox.Item>
                ))}
              </ListBox>
            </Select.Popover>
          </Select>

          {canTenant ? (
            <Switch
              size="sm"
              isSelected={tenantScope}
              onChange={setTenantScope}
              aria-label={t('integrations.market.tenantSwitchAria')}
            >
              <Switch.Content className="min-h-10">
                <Switch.Control>
                  <Switch.Thumb />
                </Switch.Control>
                <Label>{t('integrations.market.tenantScope')}</Label>
              </Switch.Content>
            </Switch>
          ) : null}
        </div>
      </div>

      {loading ? <ListSkeleton /> : null}

      {!loading && error ? <PanelError message={error} onRetry={() => void load()} /> : null}

      {!loading && !error && items.length === 0 ? (
        <PanelEmpty
          icon={<SparkIcon size={20} />}
          title={t('integrations.market.emptyTitle')}
          description={
            kind === 'all'
              ? t('integrations.market.emptyAll')
              : t('integrations.market.emptyFiltered')
          }
        />
      ) : null}

      {!loading && !error && items.length > 0 ? (
        <ul className="grid grid-cols-1 gap-3 lg:grid-cols-2">
          {items.map((item) => {
            const kindLabelKey = KIND_LABEL_KEYS[item.kind]
            const kindLabel = kindLabelKey ? t(kindLabelKey) : item.kind

            return (
              <li key={item.id}>
                <Card className="h-full">
                  <Card.Header className="flex-row items-start gap-3">
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <Card.Title className="truncate text-sm">{item.name}</Card.Title>
                        <Chip
                          size="sm"
                          variant="soft"
                          color={item.kind === 'plugin' ? 'accent' : 'success'}
                        >
                          {kindLabel}
                        </Chip>
                        <Chip size="sm" variant="soft" color="default">
                          v{item.version}
                        </Chip>
                        {item.signed ? (
                          <Chip size="sm" variant="soft" color="success">
                            {t('integrations.market.signed')}
                          </Chip>
                        ) : (
                          <Chip size="sm" variant="soft" color="warning">
                            {t('integrations.market.unsigned')}
                          </Chip>
                        )}
                      </div>
                      <Card.Description className="mt-1 text-[12px] leading-5 break-words">
                        {item.description || t('integrations.shared.noDescription')}
                      </Card.Description>
                    </div>
                  </Card.Header>

                  <Card.Content className="flex flex-col gap-3">
                    <p className="text-[11px] leading-5 break-all text-muted">
                      {t('integrations.market.published', {
                        source: item.source,
                        time: formatDateTime(item.published_at, locale),
                      })}
                    </p>
                    <div className="flex flex-wrap items-center gap-2">
                      <Button
                        variant="primary"
                        size="sm"
                        className="h-10 gap-1.5 md:h-8"
                        isPending={installingId === item.id}
                        isDisabled={installingId !== null && installingId !== item.id}
                        onPress={() => void handleInstall(item)}
                      >
                        <PlusIcon size={13} aria-hidden="true" />
                        {t('integrations.market.install')}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="h-10 gap-1.5 text-danger md:h-8"
                        onPress={() => {
                          setUninstallError(null)
                          setPendingUninstall(item)
                        }}
                      >
                        <TrashIcon size={13} aria-hidden="true" />
                        {t('integrations.market.uninstall')}
                      </Button>
                    </div>
                  </Card.Content>
                </Card>
              </li>
            )
          })}
        </ul>
      ) : null}

      {!loading && !error && items.length > 0 ? (
        <p className="flex flex-wrap items-center gap-1.5 text-[11px] text-muted">
          <CheckCircleIcon size={12} aria-hidden="true" />
          {t('integrations.market.currentScope', { scope: scopeLabel })}
          {canTenant ? t('integrations.market.currentScopeHint') : ''}
        </p>
      ) : null}

      <ConfirmDialog
        open={pendingUninstall !== null}
        title={t('integrations.market.uninstallTitle')}
        description={t('integrations.market.uninstallDescription', {
          name: pendingUninstall?.name ?? '',
          scope: scopeLabel,
        })}
        confirmLabel={t('integrations.market.uninstall')}
        isPending={uninstalling}
        error={uninstallError}
        onOpenChange={(open) => {
          if (!open) {
            setPendingUninstall(null)
            setUninstallError(null)
          }
        }}
        onConfirm={() => void handleUninstall()}
      />
    </div>
  )
}
