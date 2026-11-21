'use client'

/**
 * 插件面板。
 *
 * - 列表：name / version / source / builtin 徽标 / enabled Switch（PATCH）；
 * - builtin 插件不允许删除，删除按钮隐藏；
 * - 「从 manifest 创建」弹窗：manifest JSON 必须是对象，可选 mcp_server_id（从 MCP 列表下拉）与 enabled；
 * - 删除走确认弹窗，失败信息留在弹窗内。
 *
 * 文案全部走 i18n；manifest 解析失败的错误与文本域关联（aria-describedby），便于读屏播报。
 */

import { useCallback, useEffect, useState } from 'react'
import {
  Alert,
  Button,
  Card,
  Chip,
  Label,
  ListBox,
  Modal,
  Select,
  Switch,
  TextArea,
  toast,
} from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError, type McpServer, type Plugin } from '@/lib/types'
import { formatDateTime } from '@/lib/utils'
import { useI18n } from '@/lib/i18n'
import {
  AlertIcon,
  PencilIcon,
  PlusIcon,
  SparkIcon,
  TrashIcon,
} from '@/components/ui-bridge/icons'
import { ConfirmDialog, ListSkeleton, PanelEmpty, PanelError, parseJsonObject } from './shared'

const NO_SERVER_KEY = '__none__'

const MANIFEST_PLACEHOLDER = `{
  "name": "my-plugin",
  "version": "0.1.0",
  "kind": "mcp",
  "description": "what this plugin does",
  "mcp": { "transport": "stdio", "command": "npx", "args": ["-y", "@me/my-plugin"] },
  "tools": []
}`

const MANIFEST_ERROR_ID = 'plugin-manifest-error'

export interface PluginsPanelProps {
  /** 页面「刷新」按钮递增；面板据此重新拉取列表。 */
  refreshToken: number
}

export function PluginsPanel({ refreshToken }: PluginsPanelProps) {
  const { t, locale } = useI18n()

  const [plugins, setPlugins] = useState<Plugin[]>([])
  const [servers, setServers] = useState<McpServer[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const [togglingId, setTogglingId] = useState<string | null>(null)
  const [deletingId, setDeletingId] = useState<string | null>(null)
  const [deleteError, setDeleteError] = useState<string | null>(null)
  const [pendingDelete, setPendingDelete] = useState<Plugin | null>(null)

  // 创建弹窗
  const [createOpen, setCreateOpen] = useState(false)
  const [manifestText, setManifestText] = useState('')
  const [serverKey, setServerKey] = useState<string>(NO_SERVER_KEY)
  const [enabledDraft, setEnabledDraft] = useState(true)
  const [manifestError, setManifestError] = useState<string | null>(null)
  const [formError, setFormError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      const rows = await api.listPlugins()
      setPlugins(rows)
    } catch (cause) {
      setError(ApiError.from(cause, t('integrations.plugins.loadError')).message)
      setLoading(false)
      return
    }
    try {
      setServers(await api.listMcpServers())
    } catch {
      // 下拉数据失败不影响主列表：创建时仍可不绑定 MCP Server
      setServers([])
    }
    setLoading(false)
  }, [t])

  useEffect(() => {
    void load()
  }, [load, refreshToken])

  function openCreate() {
    setManifestText('')
    setServerKey(NO_SERVER_KEY)
    setEnabledDraft(true)
    setManifestError(null)
    setFormError(null)
    setSubmitting(false)
    setCreateOpen(true)
  }

  async function handleToggle(plugin: Plugin, next: boolean) {
    if (togglingId) return
    setTogglingId(plugin.id)
    setPlugins((prev) =>
      prev.map((item) => (item.id === plugin.id ? { ...item, enabled: next } : item)),
    )
    try {
      const updated = await api.updatePlugin(plugin.id, { enabled: next })
      setPlugins((prev) => prev.map((item) => (item.id === updated.id ? updated : item)))
      toast.success(
        next
          ? t('integrations.shared.enabledToast', { name: updated.name })
          : t('integrations.shared.disabledToast', { name: updated.name }),
      )
    } catch (cause) {
      setPlugins((prev) =>
        prev.map((item) => (item.id === plugin.id ? { ...item, enabled: plugin.enabled } : item)),
      )
      toast.danger(ApiError.from(cause, t('integrations.plugins.toggleError')).message)
    } finally {
      setTogglingId(null)
    }
  }

  async function handleDelete() {
    const target = pendingDelete
    if (!target || deletingId) return
    setDeletingId(target.id)
    setDeleteError(null)
    try {
      await api.deletePlugin(target.id)
      setPlugins((prev) => prev.filter((item) => item.id !== target.id))
      setPendingDelete(null)
      toast.success(t('integrations.shared.deletedToast', { name: target.name }))
    } catch (cause) {
      setDeleteError(ApiError.from(cause, t('integrations.plugins.deleteError')).message)
    } finally {
      setDeletingId(null)
    }
  }

  async function handleCreate(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (submitting) return

    const manifest = parseJsonObject(manifestText, 'manifest', t)
    if (!manifest.ok) {
      setManifestError(manifest.error)
      return
    }
    const manifestName = manifest.value.name
    if (typeof manifestName !== 'string' || !manifestName.trim()) {
      setManifestError(t('integrations.plugins.manifestNameRequired'))
      return
    }
    setManifestError(null)
    setFormError(null)

    setSubmitting(true)
    try {
      const created = await api.createPlugin({
        manifest: manifest.value,
        mcp_server_id: serverKey === NO_SERVER_KEY ? undefined : serverKey,
        enabled: enabledDraft,
      })
      toast.success(t('integrations.plugins.createdToast', { name: created.name }))
      setCreateOpen(false)
      await load()
    } catch (cause) {
      setFormError(ApiError.from(cause, t('integrations.plugins.createError')).message)
    } finally {
      setSubmitting(false)
    }
  }

  function serverLabel(plugin: Plugin): string | null {
    if (!plugin.mcp_server_id) return null
    const server = servers.find((item) => item.id === plugin.mcp_server_id)
    return server?.name ?? plugin.mcp_server_id
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="max-w-prose text-[13px] leading-6 text-muted">
          {t('integrations.plugins.intro')}
        </p>
        <Button
          variant="primary"
          size="sm"
          className="h-10 gap-1.5 md:h-9"
          onPress={openCreate}
          isDisabled={loading}
        >
          <PlusIcon size={15} aria-hidden="true" />
          {t('integrations.plugins.create')}
        </Button>
      </div>

      {loading ? <ListSkeleton /> : null}

      {!loading && error ? <PanelError message={error} onRetry={() => void load()} /> : null}

      {!loading && !error && plugins.length === 0 ? (
        <PanelEmpty
          icon={<SparkIcon size={20} />}
          title={t('integrations.plugins.emptyTitle')}
          description={t('integrations.plugins.emptyDescription')}
          action={
            <Button variant="primary" className="h-10 gap-1.5 px-4" onPress={openCreate}>
              <PlusIcon size={15} aria-hidden="true" />
              {t('integrations.plugins.create')}
            </Button>
          }
        />
      ) : null}

      {!loading && !error && plugins.length > 0 ? (
        <ul className="grid grid-cols-1 gap-3 lg:grid-cols-2">
          {plugins.map((plugin) => {
            const boundServer = serverLabel(plugin)

            return (
              <li key={plugin.id}>
                <Card className="h-full">
                  <Card.Header className="flex-row items-start gap-3">
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <Card.Title className="truncate font-mono text-sm">{plugin.name}</Card.Title>
                        <Chip size="sm" variant="soft" color="default">
                          v{plugin.version}
                        </Chip>
                        {plugin.builtin ? (
                          <Chip size="sm" variant="soft" color="accent">
                            {t('integrations.plugins.builtinBadge')}
                          </Chip>
                        ) : null}
                        {plugin.enabled ? null : (
                          <Chip size="sm" variant="soft" color="danger">
                            {t('integrations.shared.disabledBadge')}
                          </Chip>
                        )}
                      </div>
                      <Card.Description className="mt-1 text-[11px] break-all">
                        {boundServer
                          ? t('integrations.plugins.sourceWithServer', {
                              source: plugin.source,
                              server: boundServer,
                            })
                          : t('integrations.plugins.sourceOnly', { source: plugin.source })}
                      </Card.Description>
                    </div>
                    <Switch
                      size="sm"
                      className="shrink-0"
                      isSelected={plugin.enabled}
                      isDisabled={togglingId === plugin.id}
                      aria-label={t('integrations.shared.enableAria', { name: plugin.name })}
                      onChange={(next) => void handleToggle(plugin, next)}
                    >
                      <Switch.Content className="min-h-10">
                        <Switch.Control>
                          <Switch.Thumb />
                        </Switch.Control>
                      </Switch.Content>
                    </Switch>
                  </Card.Header>

                  <Card.Content className="flex flex-col gap-3">
                    <p className="text-[11px] leading-5 text-muted">
                      {t('integrations.shared.updatedAt', {
                        time: formatDateTime(plugin.updated_at, locale),
                      })}
                    </p>
                    <div className="flex flex-wrap items-center gap-2">
                      {plugin.builtin ? (
                        <span className="text-[11px] text-muted">
                          {t('integrations.plugins.builtinLocked')}
                        </span>
                      ) : (
                        <Button
                          variant="ghost"
                          size="sm"
                          className="h-10 gap-1.5 text-danger md:h-8"
                          onPress={() => {
                            setDeleteError(null)
                            setPendingDelete(plugin)
                          }}
                        >
                          <TrashIcon size={13} aria-hidden="true" />
                          {t('common.delete')}
                        </Button>
                      )}
                    </div>
                  </Card.Content>
                </Card>
              </li>
            )
          })}
        </ul>
      ) : null}

      {/* 从 manifest 创建 */}
      <Modal
        isOpen={createOpen}
        onOpenChange={(open) => {
          if (submitting) return
          setCreateOpen(open)
        }}
      >
        <Modal.Backdrop isDismissable={!submitting}>
          <Modal.Container placement="center" scroll="inside">
            <Modal.Dialog
              aria-label={t('integrations.plugins.dialogTitle')}
              className="w-full max-w-lg"
            >
              <Modal.Header>
                <Modal.Icon className="bg-accent-soft text-accent">
                  <PencilIcon size={18} aria-hidden="true" />
                </Modal.Icon>
                <div className="min-w-0 flex-1">
                  <Modal.Heading>{t('integrations.plugins.dialogTitle')}</Modal.Heading>
                  <p className="mt-0.5 text-xs leading-5 text-muted">
                    {t('integrations.plugins.dialogDescription')}
                  </p>
                </div>
              </Modal.Header>

              <Modal.Body>
                <form
                  id="plugin-create-form"
                  className="flex flex-col gap-4"
                  noValidate
                  onSubmit={(event) => void handleCreate(event)}
                >
                  <div className="flex flex-col gap-1.5">
                    <p className="text-[11px] font-medium tracking-wide text-subtle uppercase">
                      {t('integrations.plugins.manifestLabel')}
                    </p>
                    <TextArea
                      rows={10}
                      aria-label={t('integrations.plugins.manifestAria')}
                      aria-invalid={manifestError ? true : undefined}
                      aria-describedby={manifestError ? MANIFEST_ERROR_ID : undefined}
                      value={manifestText}
                      placeholder={MANIFEST_PLACEHOLDER}
                      onChange={(event) => {
                        setManifestText(event.target.value)
                        if (manifestError) setManifestError(null)
                      }}
                      disabled={submitting}
                      className="min-h-[200px] resize-y font-mono text-[12px]"
                    />
                  </div>
                  {manifestError ? (
                    <p
                      id={MANIFEST_ERROR_ID}
                      role="alert"
                      className="text-[12px] leading-5 break-all text-danger"
                    >
                      {manifestError}
                    </p>
                  ) : null}

                  <Select
                    isDisabled={submitting}
                    selectedKey={serverKey}
                    onSelectionChange={(key) => {
                      if (key !== null) setServerKey(String(key))
                    }}
                  >
                    <Label>{t('integrations.plugins.serverLabel')}</Label>
                    <Select.Trigger>
                      <Select.Value />
                      <Select.Indicator />
                    </Select.Trigger>
                    <Select.Popover>
                      <ListBox>
                        <ListBox.Item
                          id={NO_SERVER_KEY}
                          textValue={t('integrations.plugins.noServer')}
                        >
                          {t('integrations.plugins.noServer')}
                          <ListBox.Item.Indicator />
                        </ListBox.Item>
                        {servers.map((server) => (
                          <ListBox.Item key={server.id} id={server.id} textValue={server.name}>
                            <span className="truncate font-mono">{server.name}</span>
                            <ListBox.Item.Indicator />
                          </ListBox.Item>
                        ))}
                      </ListBox>
                    </Select.Popover>
                  </Select>

                  <Switch
                    isSelected={enabledDraft}
                    isDisabled={submitting}
                    onChange={setEnabledDraft}
                  >
                    <Switch.Content className="min-h-10">
                      <Switch.Control>
                        <Switch.Thumb />
                      </Switch.Control>
                      <Label>{t('integrations.shared.enableAfterCreate')}</Label>
                    </Switch.Content>
                  </Switch>

                  {formError ? (
                    <Alert status="danger">
                      <Alert.Indicator>
                        <AlertIcon aria-hidden="true" className="size-4" />
                      </Alert.Indicator>
                      <Alert.Content>
                        <Alert.Title>{t('integrations.plugins.formErrorTitle')}</Alert.Title>
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
                  onPress={() => setCreateOpen(false)}
                >
                  {t('common.cancel')}
                </Button>
                <Button
                  type="submit"
                  form="plugin-create-form"
                  variant="primary"
                  className="h-10 md:h-9"
                  isPending={submitting}
                >
                  {t('integrations.plugins.submit')}
                </Button>
              </Modal.Footer>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>

      <ConfirmDialog
        open={pendingDelete !== null}
        title={t('integrations.plugins.deleteTitle')}
        description={t('integrations.plugins.deleteDescription', {
          name: pendingDelete?.name ?? '',
        })}
        confirmLabel={t('common.delete')}
        isPending={deletingId !== null}
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
