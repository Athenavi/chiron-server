'use client'

/**
 * 套餐模型范围摘要。
 *
 * - 白名单为空 → 显示「套餐不限模型」；
 * - 非空 → 列出白名单条目（Chip）并说明按前缀匹配的判定口径；
 * - 文案走 `models.allowlist.*` i18n。
 */

import { Card, Chip } from '@heroui/react'
import { useI18n } from '@/lib/i18n'

export interface AllowlistSummaryProps {
  allowlist: string[]
}

export function AllowlistSummary({ allowlist }: AllowlistSummaryProps) {
  const { t } = useI18n()

  return (
    <Card className="mt-4">
      <Card.Header className="flex-row items-center gap-2">
        <Card.Title className="text-sm">{t('models.allowlist.title')}</Card.Title>
        {allowlist.length > 0 ? (
          <Chip size="sm" variant="soft" color="accent">
            {t('models.allowlist.count', { n: allowlist.length })}
          </Chip>
        ) : null}
      </Card.Header>
      <Card.Content>
        {allowlist.length === 0 ? (
          <p className="text-[12px] leading-5 text-muted">{t('models.allowlist.empty')}</p>
        ) : (
          <>
            <ul aria-label={t('models.allowlist.entries')} className="flex flex-wrap gap-1.5">
              {allowlist.map((entry) => (
                <li key={entry}>
                  <Chip size="sm" variant="soft" color="accent" className="font-mono">
                    {entry}
                  </Chip>
                </li>
              ))}
            </ul>
            <p className="mt-2 text-[11px] leading-5 text-muted">{t('models.allowlist.note')}</p>
          </>
        )}
      </Card.Content>
    </Card>
  )
}
