'use client'

/**
 * Provider 卡片：label + provider id、协议 / 密钥 / 远端发现徽标、
 * 可复制的 base_url、模型前缀列表；套餐白名单非空时额外标注允许 / 受限。
 * 文案走 `models.card.*` i18n；协议 / provider / 前缀等技术标识保持原值。
 */

import { Button, Card, Chip, toast } from '@heroui/react'
import { CopyIcon } from '@/components/ui-bridge/icons'
import { isProviderAllowed } from '@/components/models/models-meta'
import { useI18n } from '@/lib/i18n'
import type { ModelProvider } from '@/lib/types'

export interface ProviderCardProps {
  provider: ModelProvider
  /** 套餐白名单；为空表示套餐不限模型。 */
  allowlist: string[]
}

export function ProviderCard({ provider, allowlist }: ProviderCardProps) {
  const { t } = useI18n()
  const restricted = allowlist.length > 0
  const allowed = isProviderAllowed(provider, allowlist)
  const displayName = provider.label || provider.provider

  async function copyBaseUrl() {
    if (!provider.base_url) return
    try {
      await navigator.clipboard.writeText(provider.base_url)
      toast.success(t('models.card.copied'))
    } catch {
      toast.danger(t('models.card.copyFailed'))
    }
  }

  return (
    <Card className="h-full">
      <Card.Header className="flex-row items-start gap-3">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <Card.Title className="truncate text-sm">{displayName}</Card.Title>
            <span className="font-mono text-[11px] text-muted">{provider.provider}</span>
          </div>
        </div>
        {restricted ? (
          <Chip size="sm" variant="soft" color={allowed ? 'success' : 'warning'} className="shrink-0">
            {allowed ? t('models.allowlist.allowed') : t('models.allowlist.restricted')}
          </Chip>
        ) : null}
      </Card.Header>

      <Card.Content className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center gap-1.5">
          <Chip size="sm" variant="soft" color="accent">
            {t('models.card.protocol', { name: provider.protocol })}
          </Chip>
          <Chip size="sm" variant="soft" color={provider.requires_key ? 'default' : 'success'}>
            {provider.requires_key ? t('models.card.needsKey') : t('models.card.noKey')}
          </Chip>
          <Chip size="sm" variant="soft" color={provider.model_discovery ? 'success' : 'default'}>
            {provider.model_discovery ? t('models.card.discovery') : t('models.card.noDiscovery')}
          </Chip>
        </div>

        <div>
          <p className="text-[11px] text-muted">{t('models.card.baseUrl')}</p>
          {provider.base_url ? (
            <div className="mt-1 flex items-center gap-2">
              <code className="min-w-0 flex-1 truncate rounded-md bg-surface-secondary px-2 py-1.5 font-mono text-[12px] text-foreground">
                {provider.base_url}
              </code>
              <Button
                variant="ghost"
                size="sm"
                className="h-8 shrink-0 gap-1 px-2 text-[12px]"
                aria-label={t('models.card.copyAria', { name: displayName })}
                onPress={() => void copyBaseUrl()}
              >
                <CopyIcon size={13} aria-hidden="true" />
                {t('models.card.copy')}
              </Button>
            </div>
          ) : (
            <p className="mt-1 text-[12px] text-muted">{t('models.card.baseUrlMissing')}</p>
          )}
        </div>

        <div>
          <p className="text-[11px] text-muted">{t('models.card.prefixes')}</p>
          {provider.model_prefixes.length > 0 ? (
            <ul aria-label={t('models.card.prefixes')} className="mt-1 flex flex-wrap gap-1.5">
              {provider.model_prefixes.map((prefix) => (
                <li key={prefix}>
                  <Chip size="sm" variant="soft" color="default" className="font-mono">
                    {prefix}
                  </Chip>
                </li>
              ))}
            </ul>
          ) : (
            <p className="mt-1 text-[12px] text-muted">{t('models.card.prefixesEmpty')}</p>
          )}
        </div>
      </Card.Content>
    </Card>
  )
}
