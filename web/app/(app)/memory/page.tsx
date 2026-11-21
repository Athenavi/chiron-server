'use client'

/**
 * 记忆管理页（用户级 CRUD）。
 *
 * - 顶部「新建记忆 / 清空」，清空按当前槽位筛选生效并带确认；
 * - 按槽位筛选后重新拉取 `listMemories({ slot })`，加载中显示骨架，失败显示 Alert + 重试；
 * - 列表项可展开长内容、编辑（弹窗）、删除（确认弹窗）；
 * - 所有写操作成功后 toast + 重新拉取，失败展示后端 message；
 * - 全部界面文案走 i18n（`memory.*`），技术标识（scope 原值、`global`）保留原文。
 */

import { useCallback, useEffect, useRef, useState } from 'react'
import { Alert, Button, EmptyState, ListBox, Select, toast } from '@heroui/react'
import { PageHeader } from '@/components/shell/PageHeader'
import { api } from '@/lib/api'
import { ApiError, type Memory } from '@/lib/types'
import { AlertIcon, PlusIcon, RefreshIcon, SparkIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { ConfirmDialog } from '@/components/memory/ConfirmDialog'
import { MemoryFormModal, type MemorySaveMode } from '@/components/memory/MemoryFormModal'
import { MemoryItem } from '@/components/memory/MemoryItem'
import { MemoryListSkeleton } from '@/components/memory/MemoryListSkeleton'
import {
  contentPreview,
  filterOptions,
  slotLabel,
  type SlotFilter,
} from '@/components/memory/memory-meta'
import { useI18n } from '@/lib/i18n'

type LoadState = 'loading' | 'ready' | 'error'

/** 确认弹窗的两种目标：单条删除 / 按当前筛选清空。 */
type ConfirmTarget = { kind: 'one'; memory: Memory } | { kind: 'all' }

export default function MemoryPage() {
  const { t } = useI18n()
  const [filter, setFilter] = useState<SlotFilter>('all')
  const [memories, setMemories] = useState<Memory[]>([])
  const [loadState, setLoadState] = useState<LoadState>('loading')
  const [loadError, setLoadError] = useState<string | null>(null)

  const [formOpen, setFormOpen] = useState(false)
  const [editing, setEditing] = useState<Memory | null>(null)

  const [confirmTarget, setConfirmTarget] = useState<ConfirmTarget | null>(null)
  const [confirmPending, setConfirmPending] = useState(false)
  const [confirmError, setConfirmError] = useState<string | null>(null)

  /** 请求序号：筛选切换很快时只接受最后一次响应。 */
  const requestSeq = useRef(0)

  /** 「全部记忆」或「「画像」槽位」——用于空态与确认弹窗的目标描述。 */
  function filterLabel(current: SlotFilter): string {
    return current === 'all'
      ? t('memory.filter.allMemories')
      : t('memory.filter.slot', { slot: slotLabel(current, t) })
  }

  const load = useCallback(
    async (slot: SlotFilter) => {
      const seq = ++requestSeq.current
      setLoadState('loading')
      setLoadError(null)
      try {
        const rows = await api.listMemories(slot === 'all' ? {} : { slot })
        if (seq !== requestSeq.current) return
        setMemories(rows)
        setLoadState('ready')
      } catch (cause) {
        if (seq !== requestSeq.current) return
        setLoadError(ApiError.from(cause, t('memory.error.load')).message)
        setLoadState('error')
      }
    },
    [t],
  )

  useEffect(() => {
    void load(filter)
  }, [filter, load])

  function openCreate() {
    setEditing(null)
    setFormOpen(true)
  }

  function openEdit(memory: Memory) {
    setEditing(memory)
    setFormOpen(true)
  }

  function handleSaved(_saved: Memory, mode: MemorySaveMode) {
    setFormOpen(false)
    setEditing(null)
    toast.success(mode === 'create' ? t('memory.toast.created') : t('memory.toast.updated'))
    void load(filter)
  }

  function openConfirm(target: ConfirmTarget) {
    setConfirmError(null)
    setConfirmTarget(target)
  }

  function closeConfirm() {
    if (confirmPending) return
    setConfirmTarget(null)
    setConfirmError(null)
  }

  async function runConfirm() {
    const target = confirmTarget
    if (!target || confirmPending) return
    setConfirmPending(true)
    setConfirmError(null)
    try {
      if (target.kind === 'one') {
        await api.deleteMemory(target.memory.id)
        toast.success(t('memory.toast.deleted'))
      } else {
        const { deleted } = await api.clearMemories(filter === 'all' ? {} : { slot: filter })
        toast.success(
          deleted > 0
            ? t('memory.toast.cleared', { count: deleted })
            : t('memory.toast.clearEmpty'),
        )
      }
      setConfirmTarget(null)
      await load(filter)
    } catch (cause) {
      setConfirmError(ApiError.from(cause, t('memory.error.action')).message)
    } finally {
      setConfirmPending(false)
    }
  }

  const options = filterOptions(t)
  const isEmpty = loadState === 'ready' && memories.length === 0
  const clearDisabled = confirmPending || loadState !== 'ready' || memories.length === 0
  const targetLabel = filterLabel(filter)
  const confirmTitle =
    confirmTarget?.kind === 'one'
      ? t('memory.confirm.deleteTitle')
      : t('memory.confirm.clearTitle', { target: targetLabel })
  const confirmDescription =
    confirmTarget?.kind === 'one'
      ? t('memory.confirm.deleteDescription', {
          preview: contentPreview(confirmTarget.memory.content, 40),
        })
      : t('memory.confirm.clearDescription', { target: targetLabel, count: memories.length })

  return (
    <div className="flex h-[calc(100dvh-3rem)] min-h-0 flex-col bg-background md:h-dvh">
      <PageHeader
        title={t('memory.title')}
        description={t('memory.description')}
        actions={
          <>
            <Button
              variant="primary"
              size="sm"
              className="h-10 gap-1.5 md:h-9"
              onPress={openCreate}
            >
              <PlusIcon size={15} aria-hidden="true" />
              <span className="hidden sm:inline">{t('memory.actions.create')}</span>
              <span className="sm:hidden">{t('memory.actions.createShort')}</span>
            </Button>
            <Button
              variant="danger-soft"
              size="sm"
              className="h-10 gap-1.5 md:h-9"
              isDisabled={clearDisabled}
              onPress={() => openConfirm({ kind: 'all' })}
            >
              <TrashIcon size={14} aria-hidden="true" />
              {t('memory.actions.clear')}
            </Button>
          </>
        }
      />

      <div className="content-shell min-h-0 max-w-5xl flex-1 overflow-y-auto py-5 md:py-6">
        {/* 筛选 + 计数 */}
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <Select
              aria-label={t('memory.filter.aria')}
              selectedKey={filter}
              onSelectionChange={(key) => {
                if (key === null) return
                setFilter(String(key) as SlotFilter)
              }}
              className="w-40"
            >
              <Select.Trigger className="h-10 md:h-9">
                <Select.Value />
                <Select.Indicator />
              </Select.Trigger>
              <Select.Popover>
                <ListBox aria-label={t('memory.filter.listAria')}>
                  {options.map((option) => (
                    <ListBox.Item key={option.id} id={option.id} textValue={option.label}>
                      {option.label}
                      <ListBox.ItemIndicator />
                    </ListBox.Item>
                  ))}
                </ListBox>
              </Select.Popover>
            </Select>
            <span className="text-xs text-muted tabular" aria-live="polite">
              {loadState === 'ready' ? t('memory.count', { count: memories.length }) : null}
              {loadState === 'loading' ? t('memory.loading') : null}
            </span>
          </div>
        </div>

        {/* 内容区 */}
        <div className="mt-4">
          {loadState === 'loading' ? <MemoryListSkeleton /> : null}

          {loadState === 'error' ? (
            <div className="flex flex-col gap-3">
              <Alert status="danger">
                <Alert.Indicator>
                  <AlertIcon size={16} aria-hidden="true" />
                </Alert.Indicator>
                <Alert.Content>
                  <Alert.Title>{t('memory.error.loadTitle')}</Alert.Title>
                  <Alert.Description>
                    {loadError ?? t('memory.error.loadFallback')}
                  </Alert.Description>
                </Alert.Content>
              </Alert>
              <div>
                <Button
                  variant="secondary"
                  size="sm"
                  className="h-10 gap-1.5 md:h-9"
                  onPress={() => void load(filter)}
                >
                  <RefreshIcon size={14} aria-hidden="true" />
                  {t('memory.actions.retry')}
                </Button>
              </div>
            </div>
          ) : null}

          {isEmpty ? (
            <EmptyState className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-14 text-center">
              <span
                aria-hidden="true"
                className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
              >
                <SparkIcon size={20} />
              </span>
              <h2 className="text-sm font-semibold text-foreground">
                {filter === 'all'
                  ? t('memory.empty.title')
                  : t('memory.empty.titleFiltered', { target: targetLabel })}
              </h2>
              <p className="max-w-sm text-xs leading-5 text-muted">{t('memory.empty.description')}</p>
              <div className="mt-1 flex flex-wrap items-center justify-center gap-2">
                <Button variant="primary" className="h-10 gap-1.5 px-4" onPress={openCreate}>
                  <PlusIcon size={15} aria-hidden="true" />
                  {t('memory.actions.create')}
                </Button>
                {filter === 'all' ? null : (
                  <Button variant="ghost" className="h-10 px-3" onPress={() => setFilter('all')}>
                    {t('memory.actions.viewAll')}
                  </Button>
                )}
              </div>
            </EmptyState>
          ) : null}

          {loadState === 'ready' && memories.length > 0 ? (
            <ul aria-label={t('memory.list.aria')} className="flex flex-col gap-3">
              {memories.map((memory) => (
                <li key={memory.id}>
                  <MemoryItem
                    memory={memory}
                    isBusy={confirmPending}
                    onEdit={openEdit}
                    onDelete={(target) => openConfirm({ kind: 'one', memory: target })}
                  />
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      </div>

      <MemoryFormModal
        open={formOpen}
        memory={editing}
        onOpenChange={(open) => {
          setFormOpen(open)
          if (!open) setEditing(null)
        }}
        onSaved={handleSaved}
      />

      <ConfirmDialog
        open={confirmTarget !== null}
        title={confirmTitle}
        description={confirmDescription}
        confirmLabel={
          confirmTarget?.kind === 'one' ? t('memory.confirm.delete') : t('memory.confirm.clear')
        }
        isPending={confirmPending}
        error={confirmError}
        onOpenChange={(open) => {
          if (!open) closeConfirm()
        }}
        onConfirm={() => void runConfirm()}
      />
    </div>
  )
}
