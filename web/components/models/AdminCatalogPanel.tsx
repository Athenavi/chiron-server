'use client'

/**
 * 管理员 - 模型标签页：租户目录的增删与启停。
 *
 * - 列表：模型 ID、显示名、启用 Switch（直接 PATCH，乐观更新 + 失败回滚）、删除（确认）；
 * - 自动发现写入的模型额外带「自动发现」徽标，悬停展示 discovered_at；
 * - 新增弹窗：model 必填，display_name 可选（留空回落到 model）；
 * - 写操作成功/失败都 toast，失败展示后端 message。
 */

import { useCallback, useEffect, useState } from 'react'
import {
  Alert,
  Button,
  Chip,
  Description,
  FieldError,
  Input,
  Label,
  Modal,
  Switch,
  TextField,
  Tooltip,
  toast,
} from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError, type CatalogModel } from '@/lib/types'
import { formatDateTime } from '@/lib/utils'
import { useI18n } from '@/lib/i18n'
import {
  AlertIcon,
  CubeIcon,
  PlusIcon,
  SparkIcon,
  TrashIcon,
} from '@/components/ui-bridge/icons'
import { ModelsConfirmDialog, ModelsEmpty, ModelsError, ModelsSkeleton } from './models-ui'

export interface AdminCatalogPanelProps {
  refreshToken: number
}

export function AdminCatalogPanel({ refreshToken }: AdminCatalogPanelProps) {
  const { t, locale } = useI18n()

  const [models, setModels] = useState<CatalogModel[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const [toggling, setToggling] = useState<string | null>(null)
  const [pendingDelete, setPendingDelete] = useState<CatalogModel | null>(null)
  const [deleting, setDeleting] = useState(false)
  const [deleteError, setDeleteError] = useState<string | null>(null)

  const [addOpen, setAddOpen] = useState(false)
  const [modelDraft, setModelDraft] = useState('')
  const [displayDraft, setDisplayDraft] = useState('')
  const [modelError, setModelError] = useState<string | null>(null)
  const [formError, setFormError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const snapshot = await api.adminModels()
      setModels(snapshot.models)
    } catch (cause) {
      setError(ApiError.from(cause, t('models.admin.models.loadFailed')).message)
    } finally {
      setLoading(false)
    }
  }, [t])

  useEffect(() => {
    void load()
  }, [load, refreshToken])

  function openAdd() {
    setModelDraft('')
    setDisplayDraft('')
    setModelError(null)
    setFormError(null)
    setSubmitting(false)
    setAddOpen(true)
  }

  async function handleToggle(model: CatalogModel, next: boolean) {
    if (toggling) return
    setToggling(model.model)
    setModels((prev) =>
      prev.map((item) => (item.model === model.model ? { ...item, enabled: next } : item)),
    )
    try {
      const updated = await api.updateCatalogModel(model.model, { enabled: next })
      setModels((prev) => prev.map((item) => (item.model === updated.model ? updated : item)))
      toast.success(
        next
          ? t('models.admin.models.enabledToast', { name: model.display_name || model.model })
          : t('models.admin.models.disabledToast', { name: model.display_name || model.model }),
      )
    } catch (cause) {
      setModels((prev) =>
        prev.map((item) => (item.model === model.model ? { ...item, enabled: model.enabled } : item)),
      )
      toast.danger(ApiError.from(cause, t('models.admin.models.toggleFailed')).message)
    } finally {
      setToggling(null)
    }
  }

  async function handleDelete() {
    const target = pendingDelete
    if (!target || deleting) return
    setDeleting(true)
    setDeleteError(null)
    try {
      await api.deleteCatalogModel(target.model)
      setModels((prev) => prev.filter((item) => item.model !== target.model))
      setPendingDelete(null)
      toast.success(
        t('models.admin.models.deleted', { name: target.display_name || target.model }),
      )
    } catch (cause) {
      setDeleteError(ApiError.from(cause, t('models.admin.models.deleteFailed')).message)
    } finally {
      setDeleting(false)
    }
  }

  async function handleCreate(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (submitting) return

    const model = modelDraft.trim()
    if (!model) {
      setModelError(t('models.admin.models.modelRequired'))
      return
    }
    setModelError(null)
    setFormError(null)

    setSubmitting(true)
    try {
      const created = await api.addCatalogModel({
        model,
        display_name: displayDraft.trim() || undefined,
      })
      setModels((prev) => [...prev, created])
      setAddOpen(false)
      toast.success(
        t('models.admin.models.added', { name: created.display_name || created.model }),
      )
    } catch (cause) {
      setFormError(ApiError.from(cause, t('models.admin.models.addFailed')).message)
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="max-w-prose text-[13px] leading-6 text-muted">
          {t('models.admin.models.intro')}
        </p>
        <Button
          variant="primary"
          size="sm"
          className="h-10 gap-1.5 md:h-9"
          onPress={openAdd}
          isDisabled={loading}
        >
          <PlusIcon size={15} aria-hidden="true" />
          {t('models.admin.models.add')}
        </Button>
      </div>

      {loading ? (
        <ModelsSkeleton rows={3} label={t('models.admin.models.loading')} />
      ) : null}

      {!loading && error ? (
        <ModelsError
          title={t('models.admin.models.loadFailed')}
          message={error}
          onRetry={() => void load()}
        />
      ) : null}

      {!loading && !error && models.length === 0 ? (
        <ModelsEmpty
          icon={<CubeIcon size={20} />}
          title={t('models.admin.models.emptyTitle')}
          description={t('models.admin.models.emptyDescription')}
          action={
            <Button variant="primary" className="h-10 gap-1.5 px-4" onPress={openAdd}>
              <PlusIcon size={15} aria-hidden="true" />
              {t('models.admin.models.add')}
            </Button>
          }
        />
      ) : null}

      {!loading && !error && models.length > 0 ? (
        <>
          <p className="text-xs text-muted tabular">
            {t('models.admin.models.count', { n: models.length })}
          </p>
          <ul className="flex flex-col gap-2">
            {models.map((model) => (
              <li
                key={model.model}
                className="flex flex-col gap-3 rounded-lg border border-border bg-surface p-4 shadow-xs sm:flex-row sm:items-center"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="truncate font-mono text-[13px] text-foreground">{model.model}</p>
                    {model.source === 'discovered' ? (
                      <Tooltip>
                        <Tooltip.Trigger
                          className="inline-flex"
                          tabIndex={0}
                          aria-label={t('models.admin.models.discoveredAt', {
                            time: formatDateTime(model.discovered_at, locale),
                          })}
                        >
                          <Chip size="sm" variant="soft" color="accent">
                            <SparkIcon size={11} aria-hidden="true" />
                            {t('models.admin.models.discoveredBadge')}
                          </Chip>
                        </Tooltip.Trigger>
                        <Tooltip.Content>
                          {t('models.admin.models.discoveredAt', {
                            time: formatDateTime(model.discovered_at, locale),
                          })}
                        </Tooltip.Content>
                      </Tooltip>
                    ) : null}
                    {model.enabled ? null : (
                      <Chip size="sm" variant="soft" color="danger">
                        {t('common.disabled')}
                      </Chip>
                    )}
                  </div>
                  <p className="mt-0.5 truncate text-[11.5px] text-muted">
                    {t('models.admin.models.colDisplay')}：{model.display_name || model.model}
                  </p>
                  {model.source === 'discovered' ? (
                    <p className="mt-0.5 truncate text-[11px] text-muted tabular">
                      {t('models.admin.models.discoveredAt', {
                        time: formatDateTime(model.discovered_at, locale),
                      })}
                    </p>
                  ) : null}
                </div>

                <div className="flex items-center gap-2">
                  <Switch
                    size="sm"
                    isSelected={model.enabled}
                    isDisabled={toggling === model.model}
                    aria-label={t('models.admin.models.enableAria', {
                      name: model.display_name || model.model,
                    })}
                    onChange={(next) => void handleToggle(model, next)}
                  >
                    <Switch.Content className="min-h-10">
                      <Switch.Control>
                        <Switch.Thumb />
                      </Switch.Control>
                    </Switch.Content>
                  </Switch>
                  <Button
                    variant="ghost"
                    size="sm"
                    className="h-10 gap-1.5 text-danger md:h-9"
                    aria-label={t('common.delete')}
                    onPress={() => {
                      setDeleteError(null)
                      setPendingDelete(model)
                    }}
                  >
                    <TrashIcon size={14} aria-hidden="true" />
                  </Button>
                </div>
              </li>
            ))}
          </ul>
        </>
      ) : null}

      <Modal
        isOpen={addOpen}
        onOpenChange={(open) => {
          if (submitting) return
          setAddOpen(open)
        }}
      >
        <Modal.Backdrop isDismissable={!submitting}>
          <Modal.Container placement="center" scroll="inside">
            <Modal.Dialog aria-label={t('models.admin.models.addTitle')} className="w-full max-w-md">
              <Modal.Header>
                <Modal.Icon className="bg-accent-soft text-accent">
                  <CubeIcon size={18} aria-hidden="true" />
                </Modal.Icon>
                <div className="min-w-0 flex-1">
                  <Modal.Heading>{t('models.admin.models.addTitle')}</Modal.Heading>
                  <p className="mt-0.5 text-xs leading-5 text-muted">
                    {t('models.admin.models.addDescription')}
                  </p>
                </div>
              </Modal.Header>

              <Modal.Body>
                <form
                  id="catalog-model-form"
                  className="flex flex-col gap-4"
                  noValidate
                  onSubmit={(event) => void handleCreate(event)}
                >
                  <TextField
                    fullWidth
                    isRequired
                    isDisabled={submitting}
                    isInvalid={Boolean(modelError)}
                    value={modelDraft}
                    onChange={(value) => {
                      setModelDraft(value)
                      if (modelError) setModelError(null)
                    }}
                  >
                    <Label>{t('models.admin.models.modelLabel')}</Label>
                    <Input
                      autoFocus
                      autoComplete="off"
                      spellCheck={false}
                      placeholder={t('models.admin.models.modelPlaceholder')}
                      className="font-mono"
                    />
                    {modelError ? (
                      <FieldError>{modelError}</FieldError>
                    ) : (
                      <Description>{t('models.admin.models.modelHint')}</Description>
                    )}
                  </TextField>

                  <TextField
                    fullWidth
                    isDisabled={submitting}
                    value={displayDraft}
                    onChange={(value) => setDisplayDraft(value)}
                  >
                    <Label>{t('models.admin.models.displayNameLabel')}</Label>
                    <Input
                      autoComplete="off"
                      placeholder={t('models.admin.models.displayNamePlaceholder')}
                    />
                  </TextField>

                  {formError ? (
                    <Alert status="danger">
                      <Alert.Indicator>
                        <AlertIcon aria-hidden="true" className="size-4" />
                      </Alert.Indicator>
                      <Alert.Content>
                        <Alert.Title>{t('models.admin.models.addFailed')}</Alert.Title>
                        <Alert.Description className="break-all">{formError}</Alert.Description>
                      </Alert.Content>
                    </Alert>
                  ) : null}
                </form>
              </Modal.Body>

              <Modal.Footer>
                <Button
                  variant="ghost"
                  className="h-10 md:h-9"
                  isDisabled={submitting}
                  onPress={() => setAddOpen(false)}
                >
                  {t('common.cancel')}
                </Button>
                <Button
                  type="submit"
                  form="catalog-model-form"
                  variant="primary"
                  className="h-10 md:h-9"
                  isPending={submitting}
                >
                  {t('models.admin.models.submit')}
                </Button>
              </Modal.Footer>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>

      <ModelsConfirmDialog
        open={pendingDelete !== null}
        title={t('models.admin.models.deleteTitle')}
        description={t('models.admin.models.deleteDescription', {
          name: pendingDelete?.display_name || pendingDelete?.model || '',
        })}
        isPending={deleting}
        error={deleteError}
        onOpenChange={(open) => {
          if (!open) {
            setPendingDelete(null)
            setDeleteError(null)
          }
        }}
        onConfirm={() => void handleDelete()}
      />
    </div>
  )
}
