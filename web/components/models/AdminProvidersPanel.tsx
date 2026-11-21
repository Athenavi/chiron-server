'use client'

/**
 * 管理员 - Provider 与密钥标签页。
 *
 * - 按 provider 分组：preset 标签、密钥数、provider 总开关（PATCH providers）；
 * - 每个 key 一行：label、weight、更新时间、启停 Switch、删除；
 * - 「添加密钥」弹窗：provider 下拉（preset 目录 + 已有 provider）、api_key 密码框、
 *   label、weight 1-100；
 * - 自动发现：单个 provider / 全部 provider，预览后勾选落库并生成路由；
 * - 健康探测：单个 provider 即时探测，刷新路由 `last_ok_at/last_error`；
 * - 所有写操作 toast，失败展示后端 message，Switch 先乐观更新后回滚。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  Alert,
  Button,
  Card,
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
import {
  ApiError,
  type AdminModelProvider,
  type DiscoverProviderResult,
  type ModelProvider,
  type ModelProviderKey,
  type ModelRoute,
} from '@/lib/types'
import { formatDateTime } from '@/lib/utils'
import { useI18n } from '@/lib/i18n'
import {
  AlertIcon,
  CloudIcon,
  PlusIcon,
  RefreshIcon,
  SearchIcon,
  TrashIcon,
} from '@/components/ui-bridge/icons'
import { parseBoundedInt } from './models-meta'
import {
  ModelsConfirmDialog,
  ModelsError,
  ModelsHealthBadge,
  ModelsSkeleton,
} from './models-ui'
import { DiscoverModelsDialog } from './DiscoverModelsDialog'

export interface AdminProvidersPanelProps {
  refreshToken: number
  /** 发现落库 / 探测后通知父级刷新其他面板（模型、路由）。 */
  onDataChanged?: () => void
}

export function AdminProvidersPanel({ refreshToken, onDataChanged }: AdminProvidersPanelProps) {
  const { t, locale } = useI18n()

  const [providers, setProviders] = useState<AdminModelProvider[]>([])
  const [keys, setKeys] = useState<ModelProviderKey[]>([])
  const [routes, setRoutes] = useState<ModelRoute[]>([])
  const [presets, setPresets] = useState<ModelProvider[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const [providerToggling, setProviderToggling] = useState<string | null>(null)
  const [keyToggling, setKeyToggling] = useState<string | null>(null)
  const [probing, setProbing] = useState<string | null>(null)

  const [discoverOpen, setDiscoverOpen] = useState(false)
  const [discoverScope, setDiscoverScope] = useState<string | undefined>(undefined)
  const [discoverLoading, setDiscoverLoading] = useState(false)
  const [discoverError, setDiscoverError] = useState<string | null>(null)
  const [discoverResults, setDiscoverResults] = useState<DiscoverProviderResult[]>([])
  const [persisting, setPersisting] = useState(false)
  const [persistError, setPersistError] = useState<string | null>(null)
  /** 首次加载后再次刷新不闪骨架屏。 */
  const loadedRef = useRef(false)

  const [pendingDelete, setPendingDelete] = useState<ModelProviderKey | null>(null)
  const [deleting, setDeleting] = useState(false)
  const [deleteError, setDeleteError] = useState<string | null>(null)

  const [addOpen, setAddOpen] = useState(false)
  const [providerDraft, setProviderDraft] = useState('')
  const [apiKeyDraft, setApiKeyDraft] = useState('')
  const [labelDraft, setLabelDraft] = useState('')
  const [weightDraft, setWeightDraft] = useState('100')
  const [providerError, setProviderError] = useState<string | null>(null)
  const [apiKeyError, setApiKeyError] = useState<string | null>(null)
  const [weightError, setWeightError] = useState<string | null>(null)
  const [formError, setFormError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const load = useCallback(
    async (silent = false) => {
      if (!silent) setLoading(true)
      setError(null)
      try {
        const [snapshot, catalog] = await Promise.all([
          api.adminModels(),
          // preset 目录只用于下拉选项，失败不影响密钥管理本身。
          api.listModels().catch(() => ({ providers: [] as ModelProvider[] })),
        ])
        setProviders(snapshot.providers)
        setKeys(snapshot.keys)
        setRoutes(snapshot.routes ?? [])
        setPresets(catalog.providers)
        loadedRef.current = true
      } catch (cause) {
        setError(ApiError.from(cause, t('models.admin.providers.loadFailed')).message)
      } finally {
        if (!silent) setLoading(false)
      }
    },
    [t],
  )

  useEffect(() => {
    void load(loadedRef.current)
  }, [load, refreshToken])

  /** 卡片健康：优先展示最近一次错误，否则最近成功时间；均无则「未探测」。 */
  const healthOf = useCallback(
    (provider: string) => {
      const rows = routes.filter((route) => route.provider === provider)
      const failed = rows.find((route) => route.last_error?.trim())
      const latest = (pick: (route: ModelRoute) => string | null | undefined): string | null => {
        let best: string | null = null
        for (const route of rows) {
          const value = pick(route)
          if (value && (!best || value > best)) best = value
        }
        return best
      }
      return {
        lastError: failed?.last_error?.trim() || undefined,
        lastOkAt: latest((route) => route.last_ok_at),
        lastProbeAt: latest((route) => route.last_probe_at),
      }
    },
    [routes],
  )

  /** 打开发现弹窗并预览结果（persist=false，不写库）。 */
  const runDiscover = useCallback(
    async (provider?: string) => {
      setDiscoverScope(provider)
      setDiscoverOpen(true)
      setDiscoverLoading(true)
      setDiscoverError(null)
      setPersistError(null)
      setDiscoverResults([])
      try {
        const result = await api.discoverModels(provider ? { provider } : {})
        setDiscoverResults(result.results)
      } catch (cause) {
        setDiscoverError(ApiError.from(cause, t('models.admin.providers.discoverFailed')).message)
      } finally {
        setDiscoverLoading(false)
      }
    },
    [t],
  )

  /** 落库：persist=true 重新调用发现接口，再刷新面板数据。 */
  async function handlePersist() {
    if (persisting || discoverLoading) return
    setPersisting(true)
    setPersistError(null)
    try {
      const result = await api.discoverModels({
        provider: discoverScope,
        persist: true,
      })
      setDiscoverResults(result.results)
      const added = result.results.reduce(
        (sum, group) => sum + group.models.filter((item) => !item.exists).length,
        0,
      )
      setDiscoverOpen(false)
      toast.success(t('models.admin.providers.discoverPersisted', { n: added }))
      await load(true)
      onDataChanged?.()
    } catch (cause) {
      setPersistError(
        ApiError.from(cause, t('models.admin.providers.discoverPersistFailed')).message,
      )
    } finally {
      setPersisting(false)
    }
  }

  /** 健康探测：刷新 provider 下全部路由的健康字段。 */
  async function handleProbe(provider: AdminModelProvider) {
    if (probing) return
    setProbing(provider.provider)
    try {
      const result = await api.probeProvider(provider.provider)
      if (result.ok) {
        toast.success(
          t('models.admin.providers.probeOk', {
            name: provider.label,
            n: result.models ?? 0,
          }),
        )
      } else {
        const base = t('models.admin.providers.probeFailed', { name: provider.label })
        toast.danger(result.error ? `${base} (${result.error})` : base)
      }
      await load(true)
      onDataChanged?.()
    } catch (cause) {
      toast.danger(
        ApiError.from(cause, t('models.admin.providers.probeFailed', { name: provider.label }))
          .message,
      )
    } finally {
      setProbing(null)
    }
  }

  /** 下拉选项：preset 目录在前，admin 里出现但 preset 没有的 provider 追加在后。 */
  const providerOptions = useMemo(() => {
    const options: Array<{ value: string; label: string }> = []
    const seen = new Set<string>()
    for (const preset of presets) {
      if (seen.has(preset.provider)) continue
      seen.add(preset.provider)
      options.push({ value: preset.provider, label: `${preset.label} (${preset.provider})` })
    }
    for (const item of providers) {
      if (seen.has(item.provider)) continue
      seen.add(item.provider)
      options.push({ value: item.provider, label: `${item.label} (${item.provider})` })
    }
    return options
  }, [presets, providers])

  /** 分组：以 admin 返回的 provider 顺序为准，未知 provider 兜底展示。 */
  const grouped = useMemo(() => {
    const groups = providers.map((provider) => ({
      provider,
      keys: keys.filter((key) => key.provider === provider.provider),
    }))
    const known = new Set(providers.map((item) => item.provider))
    const extras = Array.from(new Set(keys.map((key) => key.provider))).filter(
      (provider) => !known.has(provider),
    )
    for (const provider of extras) {
      const row = presets.find((item) => item.provider === provider)
      groups.push({
        provider: {
          provider,
          label: row?.label ?? provider,
          keys_count: keys.filter((key) => key.provider === provider).length,
          enabled: true,
        },
        keys: keys.filter((key) => key.provider === provider),
      })
    }
    return groups
  }, [providers, keys, presets])

  function openAdd() {
    setProviderDraft(providerOptions[0]?.value ?? '')
    setApiKeyDraft('')
    setLabelDraft('')
    setWeightDraft('100')
    setProviderError(null)
    setApiKeyError(null)
    setWeightError(null)
    setFormError(null)
    setSubmitting(false)
    setAddOpen(true)
  }

  async function handleProviderToggle(provider: AdminModelProvider, next: boolean) {
    if (providerToggling) return
    setProviderToggling(provider.provider)
    setProviders((prev) =>
      prev.map((item) => (item.provider === provider.provider ? { ...item, enabled: next } : item)),
    )
    try {
      const updated = await api.setProviderEnabled(provider.provider, next)
      setProviders((prev) =>
        prev.map((item) => (item.provider === updated.provider ? updated : item)),
      )
      toast.success(
        next
          ? t('models.admin.providers.providerEnabledToast', { name: provider.label })
          : t('models.admin.providers.providerDisabledToast', { name: provider.label }),
      )
    } catch (cause) {
      setProviders((prev) =>
        prev.map((item) =>
          item.provider === provider.provider ? { ...item, enabled: provider.enabled } : item,
        ),
      )
      toast.danger(ApiError.from(cause, t('models.admin.providers.providerToggleFailed')).message)
    } finally {
      setProviderToggling(null)
    }
  }

  async function handleKeyToggle(key: ModelProviderKey, next: boolean) {
    if (keyToggling) return
    setKeyToggling(key.id)
    setKeys((prev) => prev.map((item) => (item.id === key.id ? { ...item, enabled: next } : item)))
    try {
      const updated = await api.updateProviderKey(key.id, { enabled: next })
      setKeys((prev) => prev.map((item) => (item.id === updated.id ? updated : item)))
      toast.success(
        next
          ? t('models.admin.providers.keyEnabledToast', { name: key.label })
          : t('models.admin.providers.keyDisabledToast', { name: key.label }),
      )
    } catch (cause) {
      setKeys((prev) =>
        prev.map((item) => (item.id === key.id ? { ...item, enabled: key.enabled } : item)),
      )
      toast.danger(ApiError.from(cause, t('models.admin.providers.keyToggleFailed')).message)
    } finally {
      setKeyToggling(null)
    }
  }

  async function handleDeleteKey() {
    const target = pendingDelete
    if (!target || deleting) return
    setDeleting(true)
    setDeleteError(null)
    try {
      await api.deleteProviderKey(target.id)
      setKeys((prev) => prev.filter((item) => item.id !== target.id))
      setProviders((prev) =>
        prev.map((item) =>
          item.provider === target.provider
            ? { ...item, keys_count: Math.max(0, item.keys_count - 1) }
            : item,
        ),
      )
      setPendingDelete(null)
      toast.success(t('models.admin.providers.keyDeleted', { name: target.label }))
    } catch (cause) {
      setDeleteError(ApiError.from(cause, t('models.admin.providers.keyDeleteFailed')).message)
    } finally {
      setDeleting(false)
    }
  }

  async function handleCreate(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (submitting) return

    const provider = providerDraft.trim()
    if (!provider) {
      setProviderError(t('models.admin.providers.providerRequired'))
      return
    }
    setProviderError(null)

    const apiKey = apiKeyDraft.trim()
    if (!apiKey) {
      setApiKeyError(t('models.admin.providers.apiKeyRequired'))
      return
    }
    setApiKeyError(null)

    const weight = parseBoundedInt(weightDraft, 1, 100)
    if (weight === null) {
      setWeightError(t('models.admin.providers.weightInvalid'))
      return
    }
    setWeightError(null)
    setFormError(null)

    setSubmitting(true)
    try {
      const created = await api.addProviderKey({
        provider,
        api_key: apiKey,
        label: labelDraft.trim() || undefined,
        weight,
      })
      setKeys((prev) => [...prev, created])
      setProviders((prev) =>
        prev.some((item) => item.provider === created.provider)
          ? prev.map((item) =>
              item.provider === created.provider
                ? { ...item, keys_count: item.keys_count + 1 }
                : item,
            )
          : [
              ...prev,
              {
                provider: created.provider,
                label: created.provider,
                keys_count: 1,
                enabled: true,
              },
            ],
      )
      setAddOpen(false)
      toast.success(t('models.admin.providers.added', { name: created.label }))
    } catch (cause) {
      setFormError(ApiError.from(cause, t('models.admin.providers.addFailed')).message)
    } finally {
      setSubmitting(false)
    }
  }

  /** 弹窗里的范围名：`Label (provider)`；全部发现时用通用文案。 */
  const discoverScopeLabel = useMemo(() => {
    if (!discoverScope) return t('models.admin.providers.discoverAll')
    const known =
      providers.find((item) => item.provider === discoverScope) ??
      presets.find((item) => item.provider === discoverScope)
    return known ? `${known.label} (${discoverScope})` : discoverScope
  }, [discoverScope, providers, presets, t])

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="max-w-prose text-[13px] leading-6 text-muted">
          {t('models.admin.providers.intro')}
        </p>
        <div className="flex flex-wrap items-center gap-2">
          <Button
            variant="secondary"
            size="sm"
            className="h-10 gap-1.5 md:h-9"
            onPress={() => void runDiscover(undefined)}
            isDisabled={loading || discoverLoading}
            aria-label={t('models.admin.providers.discoverAllAria')}
          >
            <SearchIcon size={15} aria-hidden="true" />
            {t('models.admin.providers.discoverAll')}
          </Button>
          <Button
            variant="primary"
            size="sm"
            className="h-10 gap-1.5 md:h-9"
            onPress={openAdd}
            isDisabled={loading}
          >
            <PlusIcon size={15} aria-hidden="true" />
            {t('models.admin.providers.addKey')}
          </Button>
        </div>
      </div>

      {loading ? (
        <ModelsSkeleton rows={2} label={t('models.admin.providers.loading')} />
      ) : null}

      {!loading && error ? (
        <ModelsError
          title={t('models.admin.providers.loadFailed')}
          message={error}
          onRetry={() => void load(false)}
        />
      ) : null}

      {!loading && !error && grouped.length === 0 ? (
        <div className="rounded-xl border border-dashed border-border bg-surface px-6 py-12 text-center">
          <p className="text-sm font-semibold text-foreground">
            {t('models.admin.providers.noKeys')}
          </p>
          <Button variant="primary" className="mt-3 h-10 gap-1.5 px-4" onPress={openAdd}>
            <PlusIcon size={15} aria-hidden="true" />
            {t('models.admin.providers.addKey')}
          </Button>
        </div>
      ) : null}

      {!loading && !error && grouped.length > 0 ? (
        <ul className="grid grid-cols-1 gap-3 xl:grid-cols-2">
          {grouped.map((group) => {
            const health = healthOf(group.provider.provider)
            return (
              <li key={group.provider.provider} className="min-w-0">
              <Card className="h-full">
                <Card.Header className="flex-row items-start gap-3">
                  <span
                    aria-hidden="true"
                    className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-surface-secondary text-muted"
                  >
                    <CloudIcon size={16} />
                  </span>
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <Card.Title className="truncate text-sm">{group.provider.label}</Card.Title>
                      <Chip size="sm" variant="soft" color="accent">
                        {group.provider.provider}
                      </Chip>
                      <Chip size="sm" variant="soft" color="default">
                        {t('models.admin.providers.keysCount', { n: group.keys.length })}
                      </Chip>
                      <ModelsHealthBadge {...health} />
                      {group.provider.enabled ? null : (
                        <Chip size="sm" variant="soft" color="danger">
                          {t('common.disabled')}
                        </Chip>
                      )}
                    </div>
                    <p className="mt-1 text-[11px] text-muted tabular">
                      {health.lastProbeAt
                        ? t('models.health.lastProbe', {
                            time: formatDateTime(health.lastProbeAt, locale),
                          })
                        : health.lastOkAt
                          ? t('models.health.lastOk', {
                              time: formatDateTime(health.lastOkAt, locale),
                            })
                          : t('models.health.neverProbed')}
                    </p>
                  </div>
                  <Switch
                    size="sm"
                    className="shrink-0"
                    isSelected={group.provider.enabled}
                    isDisabled={providerToggling === group.provider.provider}
                    aria-label={t('models.admin.providers.providerEnableAria', {
                      name: group.provider.label,
                    })}
                    onChange={(next) => void handleProviderToggle(group.provider, next)}
                  >
                    <Switch.Content className="min-h-10">
                      <Switch.Control>
                        <Switch.Thumb />
                      </Switch.Control>
                    </Switch.Content>
                  </Switch>
                </Card.Header>

                <Card.Content className="flex flex-col gap-3">
                  {group.keys.length === 0 ? (
                    <p className="text-[11.5px] leading-5 text-muted">
                      {t('models.admin.providers.noKeys')}
                    </p>
                  ) : (
                    <ul className="flex flex-col divide-y divide-border">
                      {group.keys.map((key) => (
                        <li
                          key={key.id}
                          className="flex flex-col gap-2 py-2.5 first:pt-0 last:pb-0 sm:flex-row sm:items-center sm:gap-3"
                        >
                          <div className="min-w-0 flex-1">
                            <div className="flex flex-wrap items-center gap-2">
                              <p className="truncate text-[12.5px] text-foreground">{key.label}</p>
                              {key.enabled ? null : (
                                <Chip size="sm" variant="soft" color="danger">
                                  {t('common.disabled')}
                                </Chip>
                              )}
                            </div>
                            <p className="mt-0.5 text-[11px] text-muted">
                              {t('models.admin.providers.keyMeta', {
                                weight: key.weight,
                                time: formatDateTime(key.updated_at, locale),
                              })}
                            </p>
                          </div>
                          <div className="flex items-center gap-1.5">
                            <Switch
                              size="sm"
                              isSelected={key.enabled}
                              isDisabled={keyToggling === key.id}
                              aria-label={t('models.admin.providers.keyEnableAria', {
                                name: key.label,
                              })}
                              onChange={(next) => void handleKeyToggle(key, next)}
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
                                setPendingDelete(key)
                              }}
                            >
                              <TrashIcon size={14} aria-hidden="true" />
                            </Button>
                          </div>
                        </li>
                      ))}
                    </ul>
                  )}

                  <div className="flex flex-wrap items-center gap-2 border-t border-border pt-3">
                    <Button
                      variant="secondary"
                      size="sm"
                      className="h-10 gap-1.5 md:h-9"
                      isDisabled={discoverLoading}
                      aria-label={t('models.admin.providers.discoverAria', {
                        name: group.provider.label,
                      })}
                      onPress={() => void runDiscover(group.provider.provider)}
                    >
                      <SearchIcon size={14} aria-hidden="true" />
                      {t('models.admin.providers.discover')}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-10 gap-1.5 md:h-9"
                      isPending={probing === group.provider.provider}
                      isDisabled={probing !== null && probing !== group.provider.provider}
                      aria-label={t('models.admin.providers.probeAria', {
                        name: group.provider.label,
                      })}
                      onPress={() => void handleProbe(group.provider)}
                    >
                      {probing === group.provider.provider ? null : (
                        <RefreshIcon size={14} aria-hidden="true" />
                      )}
                      {probing === group.provider.provider
                        ? t('models.admin.providers.probing')
                        : t('models.admin.providers.probe')}
                    </Button>
                  </div>
                </Card.Content>
              </Card>
              </li>
            )
          })}
        </ul>
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
            <Modal.Dialog
              aria-label={t('models.admin.providers.addTitle')}
              className="w-full max-w-md"
            >
              <Modal.Header>
                <Modal.Icon className="bg-accent-soft text-accent">
                  <CloudIcon size={18} aria-hidden="true" />
                </Modal.Icon>
                <div className="min-w-0 flex-1">
                  <Modal.Heading>{t('models.admin.providers.addTitle')}</Modal.Heading>
                  <p className="mt-0.5 text-xs leading-5 text-muted">
                    {t('models.admin.providers.addDescription')}
                  </p>
                </div>
              </Modal.Header>

              <Modal.Body>
                <form
                  id="provider-key-form"
                  className="flex flex-col gap-4"
                  noValidate
                  onSubmit={(event) => void handleCreate(event)}
                >
                  <Select
                    isDisabled={submitting}
                    isInvalid={Boolean(providerError)}
                    selectedKey={providerDraft || null}
                    onSelectionChange={(key) => {
                      setProviderDraft(key === null ? '' : String(key))
                      if (providerError) setProviderError(null)
                    }}
                  >
                    <Label>{t('models.admin.providers.providerLabel')}</Label>
                    <Select.Trigger>
                      <Select.Value />
                      <Select.Indicator />
                    </Select.Trigger>
                    <Select.Popover>
                      <ListBox>
                        {providerOptions.map((option) => (
                          <ListBox.Item
                            key={option.value}
                            id={option.value}
                            textValue={option.label}
                          >
                            {option.label}
                            <ListBox.Item.Indicator />
                          </ListBox.Item>
                        ))}
                      </ListBox>
                    </Select.Popover>
                    {providerError ? <FieldError>{providerError}</FieldError> : null}
                  </Select>

                  <TextField
                    fullWidth
                    isRequired
                    isDisabled={submitting}
                    isInvalid={Boolean(apiKeyError)}
                    value={apiKeyDraft}
                    onChange={(value) => {
                      setApiKeyDraft(value)
                      if (apiKeyError) setApiKeyError(null)
                    }}
                  >
                    <Label>{t('models.admin.providers.apiKeyLabel')}</Label>
                    <Input
                      type="password"
                      autoComplete="off"
                      spellCheck={false}
                      placeholder={t('models.admin.providers.apiKeyPlaceholder')}
                      className="font-mono"
                    />
                    {apiKeyError ? (
                      <FieldError>{apiKeyError}</FieldError>
                    ) : (
                      <Description>{t('models.admin.providers.apiKeyHint')}</Description>
                    )}
                  </TextField>

                  <div className="grid gap-4 sm:grid-cols-2">
                    <TextField
                      fullWidth
                      isDisabled={submitting}
                      value={labelDraft}
                      onChange={(value) => setLabelDraft(value)}
                    >
                      <Label>{t('models.admin.providers.labelLabel')}</Label>
                      <Input
                        autoComplete="off"
                        placeholder={t('models.admin.providers.labelPlaceholder')}
                      />
                      <Description>{t('models.admin.providers.labelHint')}</Description>
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
                      <Label>{t('models.admin.providers.weightLabel')}</Label>
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
                        <Description>{t('models.admin.providers.weightHint')}</Description>
                      )}
                    </TextField>
                  </div>

                  {formError ? (
                    <Alert status="danger">
                      <Alert.Indicator>
                        <AlertIcon aria-hidden="true" className="size-4" />
                      </Alert.Indicator>
                      <Alert.Content>
                        <Alert.Title>{t('models.admin.providers.addFailed')}</Alert.Title>
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
                  form="provider-key-form"
                  variant="primary"
                  className="h-10 md:h-9"
                  isPending={submitting}
                >
                  {t('models.admin.providers.submit')}
                </Button>
              </Modal.Footer>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>

      <ModelsConfirmDialog
        open={pendingDelete !== null}
        title={t('models.admin.providers.keyDeleteTitle')}
        description={t('models.admin.providers.keyDeleteDescription', {
          name: pendingDelete?.label ?? '',
        })}
        isPending={deleting}
        error={deleteError}
        onOpenChange={(open) => {
          if (!open) {
            setPendingDelete(null)
            setDeleteError(null)
          }
        }}
        onConfirm={() => void handleDeleteKey()}
      />

      <DiscoverModelsDialog
        open={discoverOpen}
        scopeLabel={discoverScopeLabel}
        loading={discoverLoading}
        error={discoverError}
        results={discoverResults}
        persisting={persisting}
        persistError={persistError}
        onRetry={() => void runDiscover(discoverScope)}
        onOpenChange={(open) => {
          if (!open) {
            setDiscoverOpen(false)
            setPersistError(null)
          } else {
            setDiscoverOpen(true)
          }
        }}
        onPersist={() => void handlePersist()}
      />
    </div>
  )
}
