'use client'

/**
 * MCP 面板。
 *
 * - 卡片列表：名称、transport 徽标、启用 Switch（直接 PATCH）、last_error 红色提示、最后探测时间；
 * - 卡片动作：探测（isPending，成功后 toast 工具数量并打开详情弹窗）、删除（确认）；
 * - 「添加 MCP Server」弹窗：name 必填；stdio 填 command + args（空格分隔），sse/http 填 url；
 *   env / headers 用 JSON 文本域，解析失败阻止提交；enabled 默认 true。
 *
 * 文案全部走 i18n；表单错误用 FieldError 关联到对应输入，Switch / 图标按钮都有可读标签。
 */

import { useCallback, useEffect, useState } from 'react'
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
  TextArea,
  TextField,
  toast,
} from '@heroui/react'
import { api } from '@/lib/api'
import {
  ApiError,
  type McpServer,
  type McpServerInput,
  type McpTool,
  type McpTransport,
} from '@/lib/types'
import { formatDateTime } from '@/lib/utils'
import { useI18n } from '@/lib/i18n'
import { AlertIcon, PlusIcon, TrashIcon, WrenchIcon } from '@/components/ui-bridge/icons'
import {
  ConfirmDialog,
  ListSkeleton,
  PanelEmpty,
  PanelError,
  parseJsonStringMap,
} from './shared'

const TRANSPORT_OPTIONS: Array<{ value: McpTransport; labelKey: string }> = [
  { value: 'stdio', labelKey: 'integrations.mcp.transport.stdio' },
  { value: 'sse', labelKey: 'integrations.mcp.transport.sse' },
  { value: 'http', labelKey: 'integrations.mcp.transport.http' },
]

const TEXTAREA_CLASS = 'min-h-[84px] resize-y font-mono text-[12px]'

interface ProbeDetail {
  name: string
  protocolVersion: string | null
  tools: McpTool[]
}

export interface McpPanelProps {
  /** 页面「刷新」按钮递增；面板据此重新拉取列表。 */
  refreshToken: number
}

export function McpPanel({ refreshToken }: McpPanelProps) {
  const { t, locale } = useI18n()

  const [servers, setServers] = useState<McpServer[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const [togglingId, setTogglingId] = useState<string | null>(null)
  const [probingId, setProbingId] = useState<string | null>(null)
  const [deletingId, setDeletingId] = useState<string | null>(null)
  const [deleteError, setDeleteError] = useState<string | null>(null)
  const [pendingDelete, setPendingDelete] = useState<McpServer | null>(null)
  const [detail, setDetail] = useState<ProbeDetail | null>(null)

  // 添加弹窗
  const [addOpen, setAddOpen] = useState(false)
  const [name, setName] = useState('')
  const [transport, setTransport] = useState<McpTransport>('stdio')
  const [command, setCommand] = useState('npx')
  const [argsText, setArgsText] = useState('')
  const [url, setUrl] = useState('')
  const [envText, setEnvText] = useState('')
  const [headersText, setHeadersText] = useState('')
  const [enabledDraft, setEnabledDraft] = useState(true)
  const [nameError, setNameError] = useState<string | null>(null)
  const [urlError, setUrlError] = useState<string | null>(null)
  const [formError, setFormError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      setServers(await api.listMcpServers())
    } catch (cause) {
      setError(ApiError.from(cause, t('integrations.mcp.loadError')).message)
    } finally {
      setLoading(false)
    }
  }, [t])

  useEffect(() => {
    void load()
  }, [load, refreshToken])

  function openAdd() {
    setName('')
    setTransport('stdio')
    setCommand('npx')
    setArgsText('')
    setUrl('')
    setEnvText('')
    setHeadersText('')
    setEnabledDraft(true)
    setNameError(null)
    setUrlError(null)
    setFormError(null)
    setSubmitting(false)
    setAddOpen(true)
  }

  async function handleToggle(server: McpServer, next: boolean) {
    if (togglingId) return
    setTogglingId(server.id)
    // 乐观更新：失败再回滚成原值
    setServers((prev) =>
      prev.map((item) => (item.id === server.id ? { ...item, enabled: next } : item)),
    )
    try {
      const updated = await api.updateMcpServer(server.id, { enabled: next })
      setServers((prev) => prev.map((item) => (item.id === updated.id ? updated : item)))
      toast.success(
        next
          ? t('integrations.shared.enabledToast', { name: updated.name })
          : t('integrations.shared.disabledToast', { name: updated.name }),
      )
    } catch (cause) {
      setServers((prev) =>
        prev.map((item) => (item.id === server.id ? { ...item, enabled: server.enabled } : item)),
      )
      toast.danger(ApiError.from(cause, t('integrations.mcp.toggleError')).message)
    } finally {
      setTogglingId(null)
    }
  }

  async function handleProbe(server: McpServer) {
    if (probingId) return
    setProbingId(server.id)
    try {
      const result = await api.probeMcpServer(server.id)
      toast.success(t('integrations.mcp.probeSuccess', { count: result.tools.length }))
      // 用探测结果就地更新卡片，避免整表重拉造成闪烁
      setServers((prev) =>
        prev.map((item) =>
          item.id === server.id
            ? {
                ...item,
                tools: result.tools,
                protocol_version: result.protocol_version ?? item.protocol_version,
                server_info: result.server_info ?? item.server_info,
                last_probe_at: new Date().toISOString(),
                last_error: '',
              }
            : item,
        ),
      )
      setDetail({
        name: server.name,
        protocolVersion: result.protocol_version ?? null,
        tools: result.tools,
      })
    } catch (cause) {
      toast.danger(ApiError.from(cause, t('integrations.mcp.probeError')).message)
      await load()
    } finally {
      setProbingId(null)
    }
  }

  async function handleDelete() {
    const target = pendingDelete
    if (!target || deletingId) return
    setDeletingId(target.id)
    setDeleteError(null)
    try {
      await api.deleteMcpServer(target.id)
      setServers((prev) => prev.filter((item) => item.id !== target.id))
      setPendingDelete(null)
      toast.success(t('integrations.shared.deletedToast', { name: target.name }))
    } catch (cause) {
      setDeleteError(ApiError.from(cause, t('integrations.mcp.deleteError')).message)
    } finally {
      setDeletingId(null)
    }
  }

  async function handleCreate(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (submitting) return

    const trimmedName = name.trim()
    if (!trimmedName) {
      setNameError(t('integrations.mcp.nameRequired'))
      return
    }
    setNameError(null)
    setFormError(null)

    const isStdio = transport === 'stdio'
    if (!isStdio && !url.trim()) {
      setUrlError(t('integrations.mcp.urlRequired'))
      return
    }
    setUrlError(null)

    const env = parseJsonStringMap(envText, t('integrations.mcp.envLabel'), t)
    if (!env.ok) {
      setFormError(env.error)
      return
    }
    const headers = parseJsonStringMap(headersText, t('integrations.mcp.headersLabel'), t)
    if (!headers.ok) {
      setFormError(headers.error)
      return
    }

    const body: McpServerInput = {
      name: trimmedName,
      transport,
      enabled: enabledDraft,
      args: argsText.trim() ? argsText.trim().split(/\s+/).filter(Boolean) : [],
      env: env.value,
      headers: headers.value,
      command: isStdio ? command.trim() : '',
      url: isStdio ? '' : url.trim(),
    }

    setSubmitting(true)
    try {
      const created = await api.createMcpServer(body)
      toast.success(t('integrations.mcp.addedToast', { name: created.name }))
      setAddOpen(false)
      await load()
    } catch (cause) {
      setFormError(ApiError.from(cause, t('integrations.mcp.createError')).message)
    } finally {
      setSubmitting(false)
    }
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="max-w-prose text-[13px] leading-6 text-muted">
          {t('integrations.mcp.intro')}
        </p>
        <Button
          variant="primary"
          size="sm"
          className="h-10 gap-1.5 md:h-9"
          onPress={openAdd}
          isDisabled={loading}
        >
          <PlusIcon size={15} aria-hidden="true" />
          {t('integrations.mcp.add')}
        </Button>
      </div>

      {loading ? <ListSkeleton /> : null}

      {!loading && error ? <PanelError message={error} onRetry={() => void load()} /> : null}

      {!loading && !error && servers.length === 0 ? (
        <PanelEmpty
          icon={<WrenchIcon size={20} />}
          title={t('integrations.mcp.emptyTitle')}
          description={t('integrations.mcp.emptyDescription')}
          action={
            <Button variant="primary" className="h-10 gap-1.5 px-4" onPress={openAdd}>
              <PlusIcon size={15} aria-hidden="true" />
              {t('integrations.mcp.add')}
            </Button>
          }
        />
      ) : null}

      {!loading && !error && servers.length > 0 ? (
        <ul className="grid grid-cols-1 gap-3 lg:grid-cols-2">
          {servers.map((server) => {
            const isStdio = server.transport === 'stdio'
            const endpoint = isStdio
              ? [server.command, ...server.args].filter(Boolean).join(' ') ||
                t('integrations.mcp.commandMissing')
              : server.url || t('integrations.mcp.urlMissing')

            return (
              <li key={server.id}>
                <Card className="h-full">
                  <Card.Header className="flex-row items-start gap-3">
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <Card.Title className="truncate font-mono text-sm">{server.name}</Card.Title>
                        <Chip size="sm" variant="soft" color="accent">
                          {server.transport}
                        </Chip>
                        {server.enabled ? null : (
                          <Chip size="sm" variant="soft" color="danger">
                            {t('integrations.shared.disabledBadge')}
                          </Chip>
                        )}
                      </div>
                      <Card.Description className="mt-1 font-mono text-[11px] break-all">
                        {endpoint}
                      </Card.Description>
                    </div>
                    <Switch
                      size="sm"
                      className="shrink-0"
                      isSelected={server.enabled}
                      isDisabled={togglingId === server.id}
                      aria-label={t('integrations.shared.enableAria', { name: server.name })}
                      onChange={(next) => void handleToggle(server, next)}
                    >
                      <Switch.Content className="min-h-10">
                        <Switch.Control>
                          <Switch.Thumb />
                        </Switch.Control>
                      </Switch.Content>
                    </Switch>
                  </Card.Header>

                  <Card.Content className="flex flex-col gap-3">
                    {server.last_error ? (
                      <Alert status="danger">
                        <Alert.Indicator>
                          <AlertIcon aria-hidden="true" className="size-4" />
                        </Alert.Indicator>
                        <Alert.Content>
                          <Alert.Title>{t('integrations.mcp.lastErrorTitle')}</Alert.Title>
                          <Alert.Description className="break-all">
                            {server.last_error}
                          </Alert.Description>
                        </Alert.Content>
                      </Alert>
                    ) : null}

                    <p className="text-[11px] leading-5 text-muted">
                      {t('integrations.mcp.probeStatus', {
                        time: server.last_probe_at
                          ? formatDateTime(server.last_probe_at, locale)
                          : t('integrations.mcp.neverProbed'),
                        count: server.tools.length,
                      })}
                      {server.protocol_version
                        ? ` · ${t('integrations.mcp.protocol', { version: server.protocol_version })}`
                        : ''}
                    </p>

                    <div className="flex flex-wrap items-center gap-2">
                      <Button
                        variant="secondary"
                        size="sm"
                        className="h-10 gap-1.5 md:h-8"
                        isPending={probingId === server.id}
                        isDisabled={probingId !== null && probingId !== server.id}
                        onPress={() => void handleProbe(server)}
                      >
                        <WrenchIcon size={13} aria-hidden="true" />
                        {t('integrations.mcp.probe')}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="h-10 gap-1.5 text-danger md:h-8"
                        onPress={() => {
                          setDeleteError(null)
                          setPendingDelete(server)
                        }}
                      >
                        <TrashIcon size={13} aria-hidden="true" />
                        {t('common.delete')}
                      </Button>
                    </div>
                  </Card.Content>
                </Card>
              </li>
            )
          })}
        </ul>
      ) : null}

      {/* 探测结果 */}
      <Modal
        isOpen={detail !== null}
        onOpenChange={(open) => {
          if (!open) setDetail(null)
        }}
      >
        <Modal.Backdrop>
          <Modal.Container placement="center" scroll="inside">
            <Modal.Dialog aria-label={t('integrations.mcp.detailAria')} className="w-full max-w-lg">
              <Modal.Header>
                <Modal.Icon className="bg-accent-soft text-accent">
                  <WrenchIcon size={18} aria-hidden="true" />
                </Modal.Icon>
                <div className="min-w-0 flex-1">
                  <Modal.Heading>{t('integrations.mcp.detailTitle')}</Modal.Heading>
                  <p className="mt-0.5 truncate font-mono text-[11px] text-muted">{detail?.name}</p>
                </div>
                {detail?.protocolVersion ? (
                  <Chip size="sm" variant="soft" color="default" className="shrink-0">
                    {t('integrations.mcp.protocol', { version: detail.protocolVersion })}
                  </Chip>
                ) : null}
              </Modal.Header>

              <Modal.Body>
                {detail && detail.tools.length > 0 ? (
                  <ul className="flex flex-col divide-y divide-border">
                    {detail.tools.map((tool) => (
                      <li key={tool.name} className="py-2.5 first:pt-0 last:pb-0">
                        <p className="font-mono text-[12px] break-all text-foreground">
                          {tool.name}
                        </p>
                        <p className="mt-0.5 text-[11px] leading-5 text-muted">
                          {tool.description || t('integrations.shared.noDescription')}
                        </p>
                      </li>
                    ))}
                  </ul>
                ) : (
                  <p className="text-sm text-muted">{t('integrations.mcp.detailEmpty')}</p>
                )}
              </Modal.Body>

              <Modal.Footer>
                <Button variant="primary" className="h-10 md:h-9" onPress={() => setDetail(null)}>
                  {t('common.close')}
                </Button>
              </Modal.Footer>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>

      {/* 添加 MCP Server */}
      <Modal
        isOpen={addOpen}
        onOpenChange={(open) => {
          if (submitting) return
          setAddOpen(open)
        }}
      >
        <Modal.Backdrop isDismissable={!submitting}>
          <Modal.Container placement="center" scroll="inside">
            <Modal.Dialog aria-label={t('integrations.mcp.add')} className="w-full max-w-lg">
              <Modal.Header>
                <Modal.Icon className="bg-accent-soft text-accent">
                  <WrenchIcon size={18} aria-hidden="true" />
                </Modal.Icon>
                <div className="min-w-0 flex-1">
                  <Modal.Heading>{t('integrations.mcp.add')}</Modal.Heading>
                  <p className="mt-0.5 text-xs leading-5 text-muted">
                    {t('integrations.mcp.addDescription')}
                  </p>
                </div>
              </Modal.Header>

              <Modal.Body>
                <form
                  id="mcp-server-form"
                  className="flex flex-col gap-4"
                  noValidate
                  onSubmit={(event) => void handleCreate(event)}
                >
                  <TextField
                    fullWidth
                    isRequired
                    isDisabled={submitting}
                    isInvalid={Boolean(nameError)}
                    value={name}
                    onChange={(value) => {
                      setName(value)
                      if (nameError) setNameError(null)
                    }}
                  >
                    <Label>{t('integrations.mcp.nameLabel')}</Label>
                    <Input
                      autoFocus
                      autoComplete="off"
                      spellCheck={false}
                      placeholder={t('integrations.mcp.namePlaceholder')}
                      className="font-mono"
                    />
                    {nameError ? (
                      <FieldError>{nameError}</FieldError>
                    ) : (
                      <Description>{t('integrations.mcp.nameHint')}</Description>
                    )}
                  </TextField>

                  <Select
                    isDisabled={submitting}
                    selectedKey={transport}
                    onSelectionChange={(key) => {
                      if (key !== null) setTransport(String(key) as McpTransport)
                    }}
                  >
                    <Label>{t('integrations.mcp.transportLabel')}</Label>
                    <Select.Trigger>
                      <Select.Value />
                      <Select.Indicator />
                    </Select.Trigger>
                    <Select.Popover>
                      <ListBox>
                        {TRANSPORT_OPTIONS.map((option) => (
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

                  {transport === 'stdio' ? (
                    <div className="grid gap-4 sm:grid-cols-2">
                      <TextField
                        fullWidth
                        isDisabled={submitting}
                        value={command}
                        onChange={(value) => setCommand(value)}
                      >
                        <Label>{t('integrations.mcp.commandLabel')}</Label>
                        <Input
                          autoComplete="off"
                          spellCheck={false}
                          placeholder="npx"
                          className="font-mono"
                        />
                      </TextField>

                      <TextField
                        fullWidth
                        isDisabled={submitting}
                        value={argsText}
                        onChange={(value) => setArgsText(value)}
                      >
                        <Label>{t('integrations.mcp.argsLabel')}</Label>
                        <Input
                          autoComplete="off"
                          spellCheck={false}
                          placeholder="-y @mcp/fs"
                          className="font-mono"
                        />
                        <Description>{t('integrations.mcp.argsHint')}</Description>
                      </TextField>
                    </div>
                  ) : (
                    <TextField
                      fullWidth
                      isRequired
                      isDisabled={submitting}
                      isInvalid={Boolean(urlError)}
                      value={url}
                      onChange={(value) => {
                        setUrl(value)
                        if (urlError) setUrlError(null)
                      }}
                    >
                      <Label>{t('integrations.mcp.urlLabel')}</Label>
                      <Input
                        autoComplete="off"
                        spellCheck={false}
                        placeholder="https://mcp.example.com/sse"
                        className="font-mono"
                      />
                      {urlError ? (
                        <FieldError>{urlError}</FieldError>
                      ) : (
                        <Description>{t('integrations.mcp.urlHint')}</Description>
                      )}
                    </TextField>
                  )}

                  <TextField fullWidth isDisabled={submitting}>
                    <Label>{t('integrations.mcp.envLabel')}</Label>
                    <TextArea
                      rows={3}
                      value={envText}
                      placeholder='{"API_KEY": "sk-..."}'
                      onChange={(event) => setEnvText(event.target.value)}
                      disabled={submitting}
                      className={TEXTAREA_CLASS}
                    />
                    <Description>{t('integrations.mcp.envHint')}</Description>
                  </TextField>

                  <TextField fullWidth isDisabled={submitting}>
                    <Label>{t('integrations.mcp.headersLabel')}</Label>
                    <TextArea
                      rows={3}
                      value={headersText}
                      placeholder='{"Authorization": "Bearer ..."}'
                      onChange={(event) => setHeadersText(event.target.value)}
                      disabled={submitting}
                      className={TEXTAREA_CLASS}
                    />
                    <Description>{t('integrations.mcp.headersHint')}</Description>
                  </TextField>

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
                        <Alert.Title>{t('integrations.mcp.formErrorTitle')}</Alert.Title>
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
                  form="mcp-server-form"
                  variant="primary"
                  className="h-10 md:h-9"
                  isPending={submitting}
                >
                  {t('integrations.mcp.submit')}
                </Button>
              </Modal.Footer>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>

      <ConfirmDialog
        open={pendingDelete !== null}
        title={t('integrations.mcp.deleteTitle')}
        description={t('integrations.mcp.deleteDescription', { name: pendingDelete?.name ?? '' })}
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
