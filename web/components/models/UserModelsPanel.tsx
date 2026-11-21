'use client'

/**
 * 用户视图：租户启用的模型卡片。
 *
 * - `api.listTenantModels()` 只返回启用的模型，`api.getHiddenModels()` 是当前用户的隐藏列表；
 * - 每张卡一个可见性 Switch：开 = 在选择器中启用，关 = 隐藏；先乐观更新，失败回滚并 toast；
 * - 全部隐藏时给出提示（选择器会空，但仍可手输模型名）；
 * - 无启用模型时展示空态，提示联系管理员。
 */

import { useCallback, useEffect, useState } from 'react'
import { Button, Card, Chip, Switch, toast } from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError, type TenantModel } from '@/lib/types'
import { useI18n } from '@/lib/i18n'
import { CubeIcon, RefreshIcon, SparkIcon } from '@/components/ui-bridge/icons'
import { ModelsEmpty, ModelsError, ModelsSkeleton } from './models-ui'

export interface UserModelsPanelProps {
  /** 页面「刷新」按钮递增；面板据此重新拉取。 */
  refreshToken: number
}

export function UserModelsPanel({ refreshToken }: UserModelsPanelProps) {
  const { t } = useI18n()

  const [models, setModels] = useState<TenantModel[]>([])
  const [hidden, setHidden] = useState<string[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const [pending, setPending] = useState<string | null>(null)

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const [catalog, hiddenModels] = await Promise.all([
        api.listTenantModels(),
        api.getHiddenModels(),
      ])
      setModels(catalog.models)
      setHidden(hiddenModels)
    } catch (cause) {
      setError(ApiError.from(cause, t('models.error.fallback')).message)
    } finally {
      setLoading(false)
    }
  }, [t])

  useEffect(() => {
    void load()
  }, [load, refreshToken])

  async function handleVisibilityToggle(target: TenantModel, visible: boolean) {
    if (pending) return
    setPending(target.model)
    const previous = hidden
    const nextHidden = !visible
    const next = nextHidden
      ? [...previous.filter((item) => item !== target.model), target.model]
      : previous.filter((item) => item !== target.model)
    // 乐观更新：失败回滚成原列表
    setHidden(next)
    try {
      setHidden(await api.setHiddenModels(next))
      toast.success(
        nextHidden
          ? t('models.user.hideSuccess', { name: target.display_name || target.model })
          : t('models.user.showSuccess', { name: target.display_name || target.model }),
      )
    } catch (cause) {
      setHidden(previous)
      toast.danger(ApiError.from(cause, t('models.user.hideFailed')).message)
    } finally {
      setPending(null)
    }
  }

  const hiddenSet = new Set(hidden)
  const visibleCount = models.filter((model) => !hiddenSet.has(model.model)).length
  const hiddenCount = models.length - visibleCount

  return (
    <div className="flex flex-col gap-4">
      {!loading && !error ? (
        <p className="text-xs text-muted tabular" aria-live="polite">
          {t('models.user.count', { n: models.length })}
          {hiddenCount > 0 ? ` · ${t('models.user.hiddenCount', { n: hiddenCount })}` : ''}
        </p>
      ) : null}

      {loading ? <ModelsSkeleton rows={2} /> : null}

      {!loading && error ? (
        <ModelsError
          title={t('models.error.title')}
          message={error}
          onRetry={() => void load()}
        />
      ) : null}

      {!loading && !error && models.length === 0 ? (
        <ModelsEmpty
          icon={<SparkIcon size={20} />}
          title={t('models.user.emptyTitle')}
          description={t('models.user.emptyBody')}
          action={
            <Button
              variant="secondary"
              className="h-10 gap-1.5 px-4"
              onPress={() => void load()}
            >
              <RefreshIcon size={15} />
              {t('models.user.reload')}
            </Button>
          }
        />
      ) : null}

      {!loading && !error && models.length > 0 && visibleCount === 0 ? (
        <div
          role="status"
          className="flex items-start gap-2.5 rounded-lg border border-border bg-surface-secondary px-4 py-3"
        >
          <span aria-hidden="true" className="mt-0.5 text-accent">
            <SparkIcon size={16} />
          </span>
          <div className="min-w-0">
            <p className="text-[12.5px] font-medium text-foreground">
              {t('models.user.allHiddenTitle')}
            </p>
            <p className="mt-0.5 text-[11.5px] leading-5 text-muted">
              {t('models.user.allHiddenBody')}
            </p>
          </div>
        </div>
      ) : null}

      {!loading && !error && models.length > 0 ? (
        <ul aria-label={t('models.user.listLabel')} className="grid grid-cols-1 gap-3 lg:grid-cols-2">
          {models.map((model) => {
            const isHidden = hiddenSet.has(model.model)
            const displayName = model.display_name || model.model
            return (
              <li key={model.model} className="min-w-0">
                <Card className="h-full">
                  <Card.Header className="flex-row items-start gap-3">
                    <span
                      aria-hidden="true"
                      className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-accent-soft text-accent"
                    >
                      <CubeIcon size={16} />
                    </span>
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <Card.Title className="truncate text-sm">{displayName}</Card.Title>
                        {isHidden ? (
                          <Chip size="sm" variant="soft" color="default">
                            {t('models.user.hiddenBadge')}
                          </Chip>
                        ) : null}
                      </div>
                      <Card.Description className="mt-1 truncate font-mono text-[11px]">
                        {model.model}
                      </Card.Description>
                    </div>
                  </Card.Header>

                  <Card.Content className="flex flex-wrap items-center justify-between gap-3">
                    {model.providers.length > 0 ? (
                      <Chip size="sm" variant="soft" color="accent">
                        {t('models.user.providersCount', { n: model.providers.length })}
                      </Chip>
                    ) : (
                      <span className="text-[11px] text-muted">
                        {t('models.user.providersNone')}
                      </span>
                    )}

                    <Switch
                      size="sm"
                      isSelected={!isHidden}
                      isDisabled={pending === model.model}
                      aria-label={t('models.user.visibilityAria', { name: displayName })}
                      onChange={(visible) => void handleVisibilityToggle(model, visible)}
                    >
                      <Switch.Content className="min-h-10">
                        <Switch.Control>
                          <Switch.Thumb />
                        </Switch.Control>
                      </Switch.Content>
                    </Switch>
                  </Card.Content>
                </Card>
              </li>
            )
          })}
        </ul>
      ) : null}
    </div>
  )
}
