'use client'

/**
 * 自动发现结果弹窗。
 *
 * - 按 provider 分组展示探测结果：成功组列出模型，失败组展示后端 error；
 * - 模型行可勾选（已入库的标「已存在」且不可勾选），默认勾选所有新模型；
 * - 底部「添加所选并生成路由」由父组件以 `persist=true` 重新调用发现接口；
 * - 空结果、全部失败、写入失败都有明确提示。
 */

import { useEffect, useMemo, useState } from 'react'
import { Alert, Button, Chip, Modal } from '@heroui/react'
import { AlertIcon, SearchIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import type { DiscoverProviderResult } from '@/lib/types'
import { ModelsError, ModelsSkeleton } from './models-ui'

export interface DiscoverModelsDialogProps {
  open: boolean
  /** 展示用的范围名（单个 provider 的 id 或「全部」）。 */
  scopeLabel: string
  loading: boolean
  error: string | null
  results: DiscoverProviderResult[]
  persisting: boolean
  persistError: string | null
  onRetry: () => void
  onOpenChange: (open: boolean) => void
  onPersist: () => void
}

/** 选择键：`provider:model`。 */
function optionKey(provider: string, model: string): string {
  return `${provider}::${model}`
}

export function DiscoverModelsDialog({
  open,
  scopeLabel,
  loading,
  error,
  results,
  persisting,
  persistError,
  onRetry,
  onOpenChange,
  onPersist,
}: DiscoverModelsDialogProps) {
  const { t } = useI18n()

  const [selection, setSelection] = useState<Record<string, boolean>>({})

  // 每次拿到新结果都重置选择：默认勾选所有尚未入库的模型。
  useEffect(() => {
    const next: Record<string, boolean> = {}
    for (const group of results) {
      for (const item of group.models) {
        next[optionKey(group.provider, item.model)] = !item.exists
      }
    }
    setSelection(next)
  }, [results])

  const selectedCount = useMemo(
    () => Object.values(selection).filter(Boolean).length,
    [selection],
  )

  const discoveredCount = useMemo(
    () => results.reduce((sum, group) => sum + group.models.filter((item) => !item.exists).length, 0),
    [results],
  )

  const allFailed = results.length > 0 && results.every((group) => !group.ok)

  return (
    <Modal
      isOpen={open}
      onOpenChange={(next) => {
        if (persisting && !next) return
        onOpenChange(next)
      }}
    >
      <Modal.Backdrop isDismissable={!persisting}>
        <Modal.Container placement="center" scroll="inside">
          <Modal.Dialog aria-label={t('models.admin.providers.discoverTitle')} className="w-full max-w-lg">
            <Modal.Header>
              <Modal.Icon className="bg-accent-soft text-accent">
                <SearchIcon size={18} aria-hidden="true" />
              </Modal.Icon>
              <div className="min-w-0 flex-1">
                <Modal.Heading>{t('models.admin.providers.discoverTitle')}</Modal.Heading>
                <p className="mt-0.5 text-xs leading-5 text-muted">
                  {t('models.admin.providers.discoverDescription')}
                </p>
                <p className="mt-1 truncate font-mono text-[11px] text-muted">{scopeLabel}</p>
              </div>
            </Modal.Header>

            <Modal.Body className="flex flex-col gap-3">
              {loading ? (
                <ModelsSkeleton rows={2} label={t('models.admin.providers.discoverLoading')} />
              ) : null}

              {!loading && error ? (
                <ModelsError
                  title={t('models.admin.providers.discoverFailed')}
                  message={error}
                  onRetry={onRetry}
                />
              ) : null}

              {!loading && !error && results.length === 0 ? (
                <p className="rounded-lg border border-dashed border-border bg-surface px-4 py-6 text-center text-[12.5px] leading-6 text-muted">
                  {t('models.admin.providers.discoverEmpty')}
                </p>
              ) : null}

              {!loading && !error && allFailed ? (
                <Alert status="danger">
                  <Alert.Indicator>
                    <AlertIcon aria-hidden="true" className="size-4" />
                  </Alert.Indicator>
                  <Alert.Content>
                    <Alert.Title>{t('models.admin.providers.discoverFailed')}</Alert.Title>
                    <Alert.Description>
                      {t('models.admin.providers.discoverAllFailed')}
                    </Alert.Description>
                  </Alert.Content>
                </Alert>
              ) : null}

              {!loading && !error
                ? results.map((group) => (
                    <section
                      key={group.provider}
                      className="rounded-lg border border-border bg-surface p-3 shadow-xs"
                    >
                      <div className="flex flex-wrap items-center gap-2">
                        <Chip size="sm" variant="soft" color="accent">
                          {group.provider}
                        </Chip>
                        <Chip
                          size="sm"
                          variant="soft"
                          color={group.ok ? 'success' : 'danger'}
                        >
                          {group.ok
                            ? t('models.health.ok')
                            : t('models.health.failed')}
                        </Chip>
                      </div>

                      {!group.ok ? (
                        <p className="mt-2 break-all text-[12px] leading-5 text-danger">
                          {group.error || t('models.admin.providers.discoverFailed')}
                        </p>
                      ) : group.models.length === 0 ? (
                        <p className="mt-2 text-[12px] leading-5 text-muted">
                          {t('models.admin.providers.discoverEmpty')}
                        </p>
                      ) : (
                        <ul className="mt-2 flex flex-col divide-y divide-border">
                          {group.models.map((item) => {
                            const key = optionKey(group.provider, item.model)
                            return (
                              <li key={key}>
                                <label className="flex min-h-10 cursor-pointer items-center gap-2.5 py-1.5">
                                  <input
                                    type="checkbox"
                                    className="size-4 shrink-0"
                                    style={{ accentColor: 'var(--accent)' }}
                                    checked={Boolean(selection[key])}
                                    disabled={item.exists || persisting}
                                    onChange={(event) =>
                                      setSelection((prev) => ({
                                        ...prev,
                                        [key]: event.target.checked,
                                      }))
                                    }
                                  />
                                  <span className="min-w-0 flex-1 truncate font-mono text-[12px] text-foreground">
                                    {item.model}
                                  </span>
                                  {item.exists ? (
                                    <Chip size="sm" variant="soft" color="default">
                                      {t('models.admin.providers.discoverExists')}
                                    </Chip>
                                  ) : null}
                                </label>
                              </li>
                            )
                          })}
                        </ul>
                      )}
                    </section>
                  ))
                : null}

              {persistError ? (
                <Alert status="danger">
                  <Alert.Indicator>
                    <AlertIcon aria-hidden="true" className="size-4" />
                  </Alert.Indicator>
                  <Alert.Content>
                    <Alert.Title>{t('models.admin.providers.discoverPersistFailed')}</Alert.Title>
                    <Alert.Description className="break-all">{persistError}</Alert.Description>
                  </Alert.Content>
                </Alert>
              ) : null}
            </Modal.Body>

            <Modal.Footer className="flex-wrap gap-2">
              <p className="mr-auto text-[11.5px] text-muted tabular">
                {discoveredCount === 0 && results.length > 0
                  ? t('models.admin.providers.discoverAllExists')
                  : selectedCount === 0
                    ? t('models.admin.providers.discoverNothingSelected')
                    : t('models.admin.providers.discoverSelected', { n: selectedCount })}
              </p>
              <Button
                variant="ghost"
                className="h-10 md:h-9"
                isDisabled={persisting}
                onPress={() => onOpenChange(false)}
              >
                {t('common.cancel')}
              </Button>
              <Button
                variant="primary"
                className="h-10 md:h-9"
                isPending={persisting}
                isDisabled={
                  loading || Boolean(error) || discoveredCount === 0 || selectedCount === 0
                }
                onPress={() => {
                  if (selectedCount === 0) return
                  onPersist()
                }}
              >
                {t('models.admin.providers.discoverSubmit')}
              </Button>
            </Modal.Footer>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  )
}
