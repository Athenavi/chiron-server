'use client'

/**
 * 模型选择器。
 *
 * 数据来自租户模型服务：
 * - `GET /models`（`api.listTenantModels()`）只返回租户启用的模型；
 * - 再按用户自己的隐藏列表（`api.getHiddenModels()`）过滤一遍，挂载时各拉一次；
 * - 弹窗里点模型卡片即选中该模型（`<provider>:<model>` 原样回传）；
 * - 仍保留手输模型名能力（服务端格式 `provider:model` 或裸模型名）。
 *
 * 选择结果通过 `onModelChange` 交给上层（ChatHeader → 聊天页），
 * 传 `null` 表示「使用服务端默认模型」。
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import {
  Button,
  Description,
  Input,
  Label,
  Modal,
  Skeleton,
  TextField,
  toast,
} from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError, type TenantModel } from '@/lib/types'
import { useI18n } from '@/lib/i18n'
import { ChevronDownIcon, RefreshIcon } from '@/components/ui-bridge/icons'
import { cn } from '@/lib/utils'

export interface ModelPickerProps {
  model: string | null
  isDisabled?: boolean
  onModelChange: (model: string | null) => void | Promise<void>
}

function providerOf(model: string | null): string | null {
  if (!model) return null
  const index = model.indexOf(':')
  return index > 0 ? model.slice(0, index) : null
}

/** 与后端一致：`openai:gpt-5` 与 `gpt-5` 视为同一个模型。 */
function sameModel(left: string, right: string): boolean {
  const a = left.trim().toLowerCase()
  const b = right.trim().toLowerCase()
  if (a === b) return true
  const strip = (value: string) => {
    const index = value.indexOf(':')
    return index > 0 ? value.slice(index + 1) : value
  }
  return strip(a) === strip(b)
}

export function ModelPicker({ model, isDisabled = false, onModelChange }: ModelPickerProps) {
  const { t } = useI18n()
  const [open, setOpen] = useState(false)
  const [draft, setDraft] = useState('')
  const [saving, setSaving] = useState(false)
  const [models, setModels] = useState<TenantModel[]>([])
  const [modelsLoading, setModelsLoading] = useState(true)
  const [modelsError, setModelsError] = useState<string | null>(null)
  /** 模型与隐藏列表只在挂载时拉一次；重试按钮可手动重拉。 */
  const loadedRef = useRef(false)

  const loadModels = useCallback(async () => {
    setModelsLoading(true)
    setModelsError(null)
    try {
      const [catalog, hidden] = await Promise.all([
        api.listTenantModels(),
        api.getHiddenModels(),
      ])
      const hiddenSet = new Set(hidden)
      setModels(catalog.models.filter((item) => !hiddenSet.has(item.model)))
    } catch (cause) {
      setModelsError(ApiError.from(cause, t('chat.modelPicker.loadFailed')).message)
    } finally {
      setModelsLoading(false)
    }
  }, [t])

  useEffect(() => {
    if (loadedRef.current) return
    loadedRef.current = true
    void loadModels()
  }, [loadModels])

  function openPicker() {
    setDraft(model ?? '')
    setOpen(true)
  }

  function applyModel(target: TenantModel) {
    setDraft(target.model)
  }

  const trimmedDraft = draft.trim()
  const blockedByCatalog =
    models.length > 0 &&
    trimmedDraft.length > 0 &&
    !models.some((item) => sameModel(item.model, trimmedDraft))

  async function handleSave() {
    if (saving) return
    if (blockedByCatalog) {
      toast.danger(t('chat.modelPicker.notInCatalog'))
      return
    }
    setSaving(true)
    try {
      await onModelChange(draft.trim() || null)
      setOpen(false)
      toast.success(t('chat.modelPicker.switched'))
    } catch {
      // 失败提示由聊天页负责（它会 toast 后 reject）；这里保持弹窗打开
    } finally {
      setSaving(false)
    }
  }

  const currentProvider = providerOf(model)
  const triggerLabel = model ? model : t('chat.modelPicker.defaultModel')
  const activeModel = models.find((item) => item.model === draft)

  return (
    <>
      <Button
        variant="secondary"
        size="sm"
        isDisabled={isDisabled}
        aria-label={t('chat.modelPicker.triggerAria', { model: triggerLabel })}
        onPress={openPicker}
        className="h-10 max-w-[130px] gap-1.5 rounded-full px-2.5 text-[12px] sm:h-8 sm:max-w-[200px]"
      >
        <span className="min-w-0 truncate font-mono" title={triggerLabel}>
          {triggerLabel}
        </span>
        <ChevronDownIcon size={14} className="shrink-0 opacity-60" />
      </Button>

      <Modal isOpen={open} onOpenChange={setOpen}>
        <Modal.Backdrop>
          <Modal.Container placement="center" scroll="inside">
            <Modal.Dialog aria-label={t('chat.modelPicker.title')} className="w-full max-w-md">
              <Modal.Header>
                <Modal.Heading>{t('chat.modelPicker.title')}</Modal.Heading>
              </Modal.Header>
              <Modal.Body>
                <TextField fullWidth isDisabled={saving}>
                  <Label>{t('chat.modelPicker.fieldLabel')}</Label>
                  <Input
                    autoFocus
                    value={draft}
                    placeholder={t('chat.modelPicker.placeholder')}
                    onChange={(event) => setDraft(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter') {
                        event.preventDefault()
                        void handleSave()
                      }
                    }}
                  />
                  <Description>
                    {t('chat.modelPicker.descriptionBefore')}
                    <span className="font-mono">provider:model</span>
                    {t('chat.modelPicker.descriptionAfter')}
                  </Description>
                  {blockedByCatalog ? (
                    <p role="alert" className="mt-1.5 text-[11px] leading-5 text-danger">
                      {t('chat.modelPicker.notInCatalog')}
                    </p>
                  ) : null}
                </TextField>

                <div className="mt-4">
                  <div className="flex items-center justify-between gap-2">
                    <p className="text-[12px] font-medium text-foreground">
                      {t('chat.modelPicker.models')}
                    </p>
                    {modelsError ? (
                      <Button
                        variant="ghost"
                        size="sm"
                        className="h-8 gap-1 px-2 text-[11px]"
                        onPress={() => void loadModels()}
                      >
                        <RefreshIcon size={12} />
                        {t('common.retry')}
                      </Button>
                    ) : null}
                  </div>

                  {modelsLoading ? (
                    <div className="mt-2 flex flex-col gap-2" aria-hidden="true">
                      <Skeleton className="h-11 w-full rounded-lg" />
                      <Skeleton className="h-11 w-full rounded-lg" />
                    </div>
                  ) : models.length === 0 ? (
                    <p className="mt-2 text-[12px] leading-5 text-muted">
                      {modelsError ?? t('chat.modelPicker.noModels')}
                    </p>
                  ) : (
                    <ul className="mt-2 flex max-h-64 flex-col gap-1.5 overflow-y-auto">
                      {models.map((item) => {
                        const selected = draft.trim() === item.model
                        const providerMatches =
                          item.providers.length > 0 && currentProvider === item.providers[0]
                        return (
                          <li key={item.model}>
                            <button
                              type="button"
                              onClick={() => applyModel(item)}
                              aria-pressed={selected}
                              className={cn(
                                'flex min-h-11 w-full flex-col justify-center gap-0.5 rounded-lg border px-3 py-2 text-left transition-colors',
                                selected
                                  ? 'border-accent bg-accent-soft text-accent'
                                  : 'border-border bg-surface hover:bg-surface-secondary',
                              )}
                            >
                              <span className="flex items-center gap-2">
                                <span className="truncate text-[13px] font-medium">
                                  {item.display_name || item.model}
                                </span>
                                {providerMatches ? (
                                  <span className="size-1.5 shrink-0 rounded-full bg-accent" />
                                ) : null}
                              </span>
                              <span className="truncate font-mono text-[11px] text-muted">
                                {item.model}
                              </span>
                            </button>
                          </li>
                        )
                      })}
                    </ul>
                  )}

                  {!modelsLoading && activeModel && activeModel.providers.length > 0 ? (
                    <p className="mt-2 break-words text-[11px] leading-5 text-muted">
                      {t('chat.modelPicker.modelProviders', {
                        providers: activeModel.providers.join(', '),
                      })}
                    </p>
                  ) : null}
                </div>
              </Modal.Body>
              <Modal.Footer>
                <Button variant="ghost" isDisabled={saving} onPress={() => setOpen(false)}>
                  {t('common.cancel')}
                </Button>
                <Button
                  variant="primary"
                  isPending={saving}
                  isDisabled={blockedByCatalog}
                  onPress={() => void handleSave()}
                >
                  {t('common.save')}
                </Button>
              </Modal.Footer>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>
    </>
  )
}

export default ModelPicker
