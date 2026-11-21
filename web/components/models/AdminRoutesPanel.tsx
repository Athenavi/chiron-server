'use client'

/**
 * 管理员 - 路由标签页。
 *
 * - 列表：model / provider / upstream_model / priority / weight / 健康 / 启用 Switch / 编辑 / 删除；
 * - 列表按 model、priority 升序、weight 降序排列，直观展示候选编排顺序；
 * - 新增弹窗：模型从目录下拉选择，provider 可输入，upstream_model 自动填「模型去掉
 *   `provider:` 前缀」，priority 1-1000、weight 1-100；
 * - 编辑弹窗：只改 priority / weight（PATCH 已支持），用于调整候选顺序；
 * - 写操作 toast，失败展示后端 message；Switch 乐观更新失败回滚。
 */

import { useCallback, useEffect, useMemo, useState } from 'react'
import {
  Alert,
  Button,
  Chip,
  Description,
  FieldError,
  Input,
  Label,
  ListBox,
  Modal,
  Select,
  Switch,
  TextField,
  toast,
} from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError, type CatalogModel, type ModelRoute } from '@/lib/types'
import { formatDateTime } from '@/lib/utils'
import { useI18n } from '@/lib/i18n'
import {
  AlertIcon,
  InfoIcon,
  PencilIcon,
  PlusIcon,
  WrenchIcon,
  TrashIcon,
} from '@/components/ui-bridge/icons'
import { parseBoundedInt, upstreamOf } from './models-meta'
import {
  ModelsConfirmDialog,
  ModelsEmpty,
  ModelsError,
  ModelsHealthBadge,
  ModelsSkeleton,
} from './models-ui'

export interface AdminRoutesPanelProps {
  refreshToken: number
}

export function AdminRoutesPanel({ refreshToken }: AdminRoutesPanelProps) {
  const { t, locale } = useI18n()

  const [routes, setRoutes] = useState<ModelRoute[]>([])
  const [catalog, setCatalog] = useState<CatalogModel[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const [toggling, setToggling] = useState<string | null>(null)
  const [pendingDelete, setPendingDelete] = useState<ModelRoute | null>(null)
  const [deleting, setDeleting] = useState(false)
  const [deleteError, setDeleteError] = useState<string | null>(null)

  const [addOpen, setAddOpen] = useState(false)
  const [modelDraft, setModelDraft] = useState('')
  const [providerDraft, setProviderDraft] = useState('')
  const [upstreamDraft, setUpstreamDraft] = useState('')
  const [upstreamDirty, setUpstreamDirty] = useState(false)
  const [priorityDraft, setPriorityDraft] = useState('100')
  const [weightDraft, setWeightDraft] = useState('100')
  const [modelError, setModelError] = useState<string | null>(null)
  const [providerError, setProviderError] = useState<string | null>(null)
  const [priorityError, setPriorityError] = useState<string | null>(null)
  const [weightError, setWeightError] = useState<string | null>(null)
  const [formError, setFormError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const [editing, setEditing] = useState<ModelRoute | null>(null)
  const [priorityEdit, setPriorityEdit] = useState('100')
  const [weightEdit, setWeightEdit] = useState('100')
  const [editPriorityError, setEditPriorityError] = useState<string | null>(null)
  const [editWeightError, setEditWeightError] = useState<string | null>(null)
  const [editError, setEditError] = useState<string | null>(null)
  const [editSubmitting, setEditSubmitting] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const snapshot = await api.adminModels()
      setRoutes(snapshot.routes)
      setCatalog(snapshot.models)
    } catch (cause) {
      setError(ApiError.from(cause, t('models.admin.routes.loadFailed')).message)
    } finally {
      setLoading(false)
    }
  }, [t])

  useEffect(() => {
    void load()
  }, [load, refreshToken])

  const routeLabel = useCallback((route: ModelRoute) => `${route.model} → ${route.provider}`, [])

  function openAdd() {
    const first = catalog[0]?.model ?? ''
    setModelDraft(first)
    setProviderDraft('')
    setUpstreamDraft(first ? upstreamOf(first) : '')
    setUpstreamDirty(false)
    setPriorityDraft('100')
    setWeightDraft('100')
    setModelError(null)
    setProviderError(null)
    setPriorityError(null)
    setWeightError(null)
    setFormError(null)
    setSubmitting(false)
    setAddOpen(true)
  }

  function applyModel(model: string) {
    setModelDraft(model)
    if (modelError) setModelError(null)
    if (!upstreamDirty) setUpstreamDraft(model ? upstreamOf(model) : '')
  }

  async function handleToggle(route: ModelRoute, next: boolean) {
    if (toggling) return
    setToggling(route.id)
    setRoutes((prev) => prev.map((item) => (item.id === route.id ? { ...item, enabled: next } : item)))
    try {
      const updated = await api.updateModelRoute(route.id, { enabled: next })
      // 合并：PATCH 响应可能不带健康字段，保留旧值避免健康列闪回「未探测」。
      setRoutes((prev) =>
        prev.map((item) => (item.id === updated.id ? { ...item, ...updated } : item)),
      )
      toast.success(
        next
          ? t('models.admin.routes.enabledToast', { name: routeLabel(route) })
          : t('models.admin.routes.disabledToast', { name: routeLabel(route) }),
      )
    } catch (cause) {
      setRoutes((prev) =>
        prev.map((item) => (item.id === route.id ? { ...item, enabled: route.enabled } : item)),
      )
      toast.danger(ApiError.from(cause, t('models.admin.routes.toggleFailed')).message)
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
      await api.deleteModelRoute(target.id)
      setRoutes((prev) => prev.filter((item) => item.id !== target.id))
      setPendingDelete(null)
      toast.success(t('models.admin.routes.deleted', { name: routeLabel(target) }))
    } catch (cause) {
      setDeleteError(ApiError.from(cause, t('models.admin.routes.deleteFailed')).message)
    } finally {
      setDeleting(false)
    }
  }

  async function handleCreate(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (submitting) return

    const model = modelDraft.trim()
    if (!model) {
      setModelError(t('models.admin.routes.modelRequired'))
      return
    }
    setModelError(null)

    const provider = providerDraft.trim()
    if (!provider) {
      setProviderError(t('models.admin.routes.providerRequired'))
      return
    }
    setProviderError(null)

    const priority = parseBoundedInt(priorityDraft, 1, 1000)
    if (priority === null) {
      setPriorityError(t('models.admin.routes.priorityInvalid'))
      return
    }
    setPriorityError(null)

    const weight = parseBoundedInt(weightDraft, 1, 100)
    if (weight === null) {
      setWeightError(t('models.admin.routes.weightInvalid'))
      return
    }
    setWeightError(null)
    setFormError(null)

    setSubmitting(true)
    try {
      const created = await api.addModelRoute({
        model,
        provider,
        upstream_model: upstreamDraft.trim() || undefined,
        priority,
        weight,
      })
      setRoutes((prev) => [...prev, created])
      setAddOpen(false)
      toast.success(t('models.admin.routes.added', { name: routeLabel(created) }))
    } catch (cause) {
      setFormError(ApiError.from(cause, t('models.admin.routes.addFailed')).message)
    } finally {
      setSubmitting(false)
    }
  }

  const catalogOptions = useMemo(() => catalog.map((item) => item.model), [catalog])

  /** 展示顺序 = 尝试顺序：同一模型内按 priority 升序、weight 降序。 */
  const orderedRoutes = useMemo(
    () =>
      [...routes].sort(
        (left, right) =>
          left.model.localeCompare(right.model) ||
          left.priority - right.priority ||
          right.weight - left.weight,
      ),
    [routes],
  )

  function openEdit(route: ModelRoute) {
    setEditing(route)
    setPriorityEdit(String(route.priority))
    setWeightEdit(String(route.weight))
    setEditPriorityError(null)
    setEditWeightError(null)
    setEditError(null)
    setEditSubmitting(false)
  }

  async function handleEdit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const target = editing
    if (!target || editSubmitting) return

    const priority = parseBoundedInt(priorityEdit, 1, 1000)
    if (priority === null) {
      setEditPriorityError(t('models.admin.routes.priorityInvalid'))
      return
    }
    setEditPriorityError(null)

    const weight = parseBoundedInt(weightEdit, 1, 100)
    if (weight === null) {
      setEditWeightError(t('models.admin.routes.weightInvalid'))
      return
    }
    setEditWeightError(null)
    setEditError(null)

    setEditSubmitting(true)
    try {
      const updated = await api.updateModelRoute(target.id, { priority, weight })
      setRoutes((prev) =>
        prev.map((item) => (item.id === updated.id ? { ...item, ...updated } : item)),
      )
      setEditing(null)
      toast.success(t('models.admin.routes.edited', { name: routeLabel(updated) }))
    } catch (cause) {
      setEditError(ApiError.from(cause, t('models.admin.routes.editFailed')).message)
    } finally {
      setEditSubmitting(false)
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="max-w-prose text-[13px] leading-6 text-muted">
          {t('models.admin.routes.intro')}
        </p>
        <Button
          variant="primary"
          size="sm"
          className="h-10 gap-1.5 md:h-9"
          onPress={openAdd}
          isDisabled={loading || catalog.length === 0}
        >
          <PlusIcon size={15} aria-hidden="true" />
          {t('models.admin.routes.add')}
        </Button>
      </div>

      {loading ? (
        <ModelsSkeleton rows={2} label={t('models.admin.routes.loading')} />
      ) : null}

      {!loading && error ? (
        <ModelsError
          title={t('models.admin.routes.loadFailed')}
          message={error}
          onRetry={() => void load()}
        />
      ) : null}

      {!loading && !error && routes.length === 0 ? (
        <ModelsEmpty
          icon={<WrenchIcon size={20} />}
          title={t('models.admin.routes.emptyTitle')}
          description={t('models.admin.routes.emptyDescription')}
          action={
            <Button
              variant="primary"
              className="h-10 gap-1.5 px-4"
              isDisabled={catalog.length === 0}
              onPress={openAdd}
            >
              <PlusIcon size={15} aria-hidden="true" />
              {t('models.admin.routes.add')}
            </Button>
          }
        />
      ) : null}

      {!loading && !error && routes.length > 0 ? (
        <>
          <p className="text-xs text-muted tabular">
            {t('models.admin.routes.count', { n: routes.length })}
          </p>
          <p className="flex items-start gap-2 rounded-lg border border-border bg-surface-secondary px-3 py-2 text-[11.5px] leading-5 text-muted">
            <InfoIcon size={14} aria-hidden="true" className="mt-0.5 shrink-0" />
            <span>{t('models.admin.routes.orderingHint')}</span>
          </p>
          <ul className="flex flex-col gap-2">
            {orderedRoutes.map((route) => (
              <li
                key={route.id}
                className="flex flex-col gap-3 rounded-lg border border-border bg-surface p-4 shadow-xs lg:flex-row lg:items-center"
              >
                <div className="min-w-0 flex-1">
                  <div className="flex flex-wrap items-center gap-2">
                    <p className="truncate font-mono text-[13px] text-foreground">{route.model}</p>
                    <Chip size="sm" variant="soft" color="accent">
                      {route.provider}
                    </Chip>
                    {route.enabled ? null : (
                      <Chip size="sm" variant="soft" color="danger">
                        {t('common.disabled')}
                      </Chip>
                    )}
                  </div>
                  <p className="mt-0.5 truncate font-mono text-[11px] text-muted">
                    {t('models.admin.routes.colUpstream')}：{route.upstream_model}
                  </p>
                </div>

                <div className="flex flex-wrap items-center gap-x-4 gap-y-2">
                  <span className="text-[11px] text-muted tabular">
                    {t('models.admin.routes.colPriority')} {route.priority}
                  </span>
                  <span className="text-[11px] text-muted tabular">
                    {t('models.admin.routes.colWeight')} {route.weight}
                  </span>
                  <div
                    className="flex flex-wrap items-center gap-2"
                    role="group"
                    aria-label={t('models.admin.routes.colHealth')}
                  >
                    <ModelsHealthBadge
                      lastOkAt={route.last_ok_at}
                      lastProbeAt={route.last_probe_at}
                      lastError={route.last_error}
                    />
                    <span className="text-[11px] text-muted tabular">
                      {route.last_probe_at
                        ? t('models.health.lastProbe', {
                            time: formatDateTime(route.last_probe_at, locale),
                          })
                        : t('models.health.neverProbed')}
                    </span>
                  </div>
                  <div className="flex items-center gap-1.5">
                    <Switch
                      size="sm"
                      isSelected={route.enabled}
                      isDisabled={toggling === route.id}
                      aria-label={t('models.admin.routes.enableAria', {
                        model: route.model,
                        provider: route.provider,
                      })}
                      onChange={(next) => void handleToggle(route, next)}
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
                      className="h-10 gap-1.5 md:h-9"
                      aria-label={t('models.admin.routes.editAria', {
                        model: route.model,
                        provider: route.provider,
                      })}
                      onPress={() => openEdit(route)}
                    >
                      <PencilIcon size={14} aria-hidden="true" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-10 gap-1.5 text-danger md:h-9"
                      aria-label={t('common.delete')}
                      onPress={() => {
                        setDeleteError(null)
                        setPendingDelete(route)
                      }}
                    >
                      <TrashIcon size={14} aria-hidden="true" />
                    </Button>
                  </div>
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
            <Modal.Dialog aria-label={t('models.admin.routes.addTitle')} className="w-full max-w-lg">
              <Modal.Header>
                <Modal.Icon className="bg-accent-soft text-accent">
                  <WrenchIcon size={18} aria-hidden="true" />
                </Modal.Icon>
                <div className="min-w-0 flex-1">
                  <Modal.Heading>{t('models.admin.routes.addTitle')}</Modal.Heading>
                  <p className="mt-0.5 text-xs leading-5 text-muted">
                    {t('models.admin.routes.addDescription')}
                  </p>
                </div>
              </Modal.Header>

              <Modal.Body>
                <form
                  id="model-route-form"
                  className="flex flex-col gap-4"
                  noValidate
                  onSubmit={(event) => void handleCreate(event)}
                >
                  <Select
                    isDisabled={submitting}
                    isInvalid={Boolean(modelError)}
                    selectedKey={modelDraft || null}
                    onSelectionChange={(key) => applyModel(key === null ? '' : String(key))}
                  >
                    <Label>{t('models.admin.routes.modelLabel')}</Label>
                    <Select.Trigger>
                      <Select.Value />
                      <Select.Indicator />
                    </Select.Trigger>
                    <Select.Popover>
                      <ListBox>
                        {catalogOptions.map((model) => (
                          <ListBox.Item key={model} id={model} textValue={model}>
                            <span className="font-mono text-[12px]">{model}</span>
                            <ListBox.Item.Indicator />
                          </ListBox.Item>
                        ))}
                      </ListBox>
                    </Select.Popover>
                    {modelError ? <FieldError>{modelError}</FieldError> : null}
                  </Select>

                  <div className="grid gap-4 sm:grid-cols-2">
                    <TextField
                      fullWidth
                      isRequired
                      isDisabled={submitting}
                      isInvalid={Boolean(providerError)}
                      value={providerDraft}
                      onChange={(value) => {
                        setProviderDraft(value)
                        if (providerError) setProviderError(null)
                      }}
                    >
                      <Label>{t('models.admin.routes.providerLabel')}</Label>
                      <Input
                        autoComplete="off"
                        spellCheck={false}
                        placeholder={t('models.admin.routes.providerPlaceholder')}
                        className="font-mono"
                      />
                      {providerError ? <FieldError>{providerError}</FieldError> : null}
                    </TextField>

                    <TextField
                      fullWidth
                      isDisabled={submitting}
                      value={upstreamDraft}
                      onChange={(value) => {
                        setUpstreamDraft(value)
                        setUpstreamDirty(value.trim().length > 0)
                      }}
                    >
                      <Label>{t('models.admin.routes.upstreamLabel')}</Label>
                      <Input
                        autoComplete="off"
                        spellCheck={false}
                        placeholder={modelDraft ? upstreamOf(modelDraft) : ''}
                        className="font-mono"
                      />
                      <Description>{t('models.admin.routes.upstreamHint')}</Description>
                    </TextField>
                  </div>

                  <div className="grid gap-4 sm:grid-cols-2">
                    <TextField
                      fullWidth
                      isDisabled={submitting}
                      isInvalid={Boolean(priorityError)}
                      value={priorityDraft}
                      onChange={(value) => {
                        setPriorityDraft(value)
                        if (priorityError) setPriorityError(null)
                      }}
                    >
                      <Label>{t('models.admin.routes.priorityLabel')}</Label>
                      <Input
                        type="number"
                        inputMode="numeric"
                        min={1}
                        max={1000}
                        step={1}
                        autoComplete="off"
                      />
                      {priorityError ? (
                        <FieldError>{priorityError}</FieldError>
                      ) : (
                        <Description>{t('models.admin.routes.priorityHint')}</Description>
                      )}
                    </TextField>

                    <TextField
                      fullWidth
                      isDisabled={submitting}
                      isInvalid={Boolean(weightError)}
                      value={weightDraft}
                      onChange={(value) => {
                        setWeightDraft(value)
                        if (weightError) setWeightError(null)
                      }}
                    >
                      <Label>{t('models.admin.routes.weightLabel')}</Label>
                      <Input
                        type="number"
                        inputMode="numeric"
                        min={1}
                        max={100}
                        step={1}
                        autoComplete="off"
                      />
                      {weightError ? (
                        <FieldError>{weightError}</FieldError>
                      ) : (
                        <Description>{t('models.admin.routes.weightHint')}</Description>
                      )}
                    </TextField>
                  </div>

                  {formError ? (
                    <Alert status="danger">
                      <Alert.Indicator>
                        <AlertIcon aria-hidden="true" className="size-4" />
                      </Alert.Indicator>
                      <Alert.Content>
                        <Alert.Title>{t('models.admin.routes.addFailed')}</Alert.Title>
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
                  form="model-route-form"
                  variant="primary"
                  className="h-10 md:h-9"
                  isPending={submitting}
                >
                  {t('models.admin.routes.submit')}
                </Button>
              </Modal.Footer>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>

      <Modal
        isOpen={editing !== null}
        onOpenChange={(open) => {
          if (editSubmitting) return
          if (!open) setEditing(null)
        }}
      >
        <Modal.Backdrop isDismissable={!editSubmitting}>
          <Modal.Container placement="center" scroll="inside">
            <Modal.Dialog aria-label={t('models.admin.routes.editTitle')} className="w-full max-w-md">
              <Modal.Header>
                <Modal.Icon className="bg-accent-soft text-accent">
                  <PencilIcon size={18} aria-hidden="true" />
                </Modal.Icon>
                <div className="min-w-0 flex-1">
                  <Modal.Heading>{t('models.admin.routes.editTitle')}</Modal.Heading>
                  <p className="mt-0.5 text-xs leading-5 text-muted">
                    {t('models.admin.routes.editDescription')}
                  </p>
                  <p className="mt-1 truncate font-mono text-[11px] text-muted">
                    {editing ? routeLabel(editing) : ''}
                  </p>
                </div>
              </Modal.Header>

              <Modal.Body>
                <form
                  id="model-route-edit-form"
                  className="flex flex-col gap-4"
                  noValidate
                  onSubmit={(event) => void handleEdit(event)}
                >
                  <div className="grid gap-4 sm:grid-cols-2">
                    <TextField
                      fullWidth
                      isDisabled={editSubmitting}
                      isInvalid={Boolean(editPriorityError)}
                      value={priorityEdit}
                      onChange={(value) => {
                        setPriorityEdit(value)
                        if (editPriorityError) setEditPriorityError(null)
                      }}
                    >
                      <Label>{t('models.admin.routes.priorityLabel')}</Label>
                      <Input
                        type="number"
                        inputMode="numeric"
                        min={1}
                        max={1000}
                        step={1}
                        autoComplete="off"
                      />
                      {editPriorityError ? (
                        <FieldError>{editPriorityError}</FieldError>
                      ) : (
                        <Description>{t('models.admin.routes.priorityHint')}</Description>
                      )}
                    </TextField>

                    <TextField
                      fullWidth
                      isDisabled={editSubmitting}
                      isInvalid={Boolean(editWeightError)}
                      value={weightEdit}
                      onChange={(value) => {
                        setWeightEdit(value)
                        if (editWeightError) setEditWeightError(null)
                      }}
                    >
                      <Label>{t('models.admin.routes.weightLabel')}</Label>
                      <Input
                        type="number"
                        inputMode="numeric"
                        min={1}
                        max={100}
                        step={1}
                        autoComplete="off"
                      />
                      {editWeightError ? (
                        <FieldError>{editWeightError}</FieldError>
                      ) : (
                        <Description>{t('models.admin.routes.weightHint')}</Description>
                      )}
                    </TextField>
                  </div>

                  {editError ? (
                    <Alert status="danger">
                      <Alert.Indicator>
                        <AlertIcon aria-hidden="true" className="size-4" />
                      </Alert.Indicator>
                      <Alert.Content>
                        <Alert.Title>{t('models.admin.routes.editFailed')}</Alert.Title>
                        <Alert.Description className="break-all">{editError}</Alert.Description>
                      </Alert.Content>
                    </Alert>
                  ) : null}
                </form>
              </Modal.Body>

              <Modal.Footer>
                <Button
                  variant="ghost"
                  className="h-10 md:h-9"
                  isDisabled={editSubmitting}
                  onPress={() => setEditing(null)}
                >
                  {t('common.cancel')}
                </Button>
                <Button
                  type="submit"
                  form="model-route-edit-form"
                  variant="primary"
                  className="h-10 md:h-9"
                  isPending={editSubmitting}
                >
                  {t('models.admin.routes.editSubmit')}
                </Button>
              </Modal.Footer>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>

      <ModelsConfirmDialog
        open={pendingDelete !== null}
        title={t('models.admin.routes.deleteTitle')}
        description={t('models.admin.routes.deleteDescription', {
          name: pendingDelete ? routeLabel(pendingDelete) : '',
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
