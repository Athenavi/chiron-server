'use client'

/**
 * 集成页四个面板（MCP / 插件 / 技能 / 市场）共用的基础件：
 * - 列表加载骨架、空态、错误态（Alert + 重试）；
 * - 危险操作确认弹窗（AlertDialog，失败信息留在弹窗内便于重试）；
 * - JSON 文本域校验工具（必须是对象 / 必须是字符串映射）。
 *
 * 所有可见文案走 i18n；校验工具接收面板传入的 `t`，保证错误文案随语言切换。
 */

import type { ReactNode } from 'react'
import { Alert, AlertDialog, Button, EmptyState, Skeleton } from '@heroui/react'
import { AlertIcon, RefreshIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'

/** `useI18n().t` 的结构类型，便于把翻译函数传进纯函数工具。 */
export type Translate = (key: string, vars?: Record<string, string | number>) => string

/** 列表骨架：3 行卡片，避免首屏跳动。 */
export function ListSkeleton({ rows = 3 }: { rows?: number }) {
  const { t } = useI18n()

  return (
    <div role="status" aria-live="polite" aria-busy="true" className="flex flex-col gap-3">
      <span className="sr-only">{t('integrations.shared.loadingList')}</span>
      {Array.from({ length: rows }, (_, index) => (
        <div
          key={index}
          className="rounded-lg border border-border bg-surface p-4 shadow-xs"
          aria-hidden="true"
        >
          <div className="flex items-center gap-2">
            <Skeleton className="h-3.5 w-32" />
            <Skeleton className="h-5 w-14 rounded-full" />
            <Skeleton className="ml-auto h-5 w-10 rounded-full" />
          </div>
          <Skeleton className="mt-3 h-2.5" style={{ width: `${58 + index * 8}%` }} />
          <Skeleton className="mt-2 h-2.5 w-1/4" />
        </div>
      ))}
    </div>
  )
}

/** 面板空态：图标 + 标题 + 说明 + 可选主操作。 */
export function PanelEmpty({
  icon,
  title,
  description,
  action,
}: {
  icon: ReactNode
  title: string
  description: string
  action?: ReactNode
}) {
  return (
    <EmptyState className="flex flex-col items-center gap-3 rounded-xl border border-dashed border-border bg-surface px-6 py-12 text-center">
      <span
        aria-hidden="true"
        className="flex size-11 items-center justify-center rounded-full bg-surface-secondary text-muted"
      >
        {icon}
      </span>
      <h2 className="text-sm font-semibold text-foreground">{title}</h2>
      <p className="max-w-sm text-xs leading-5 text-muted">{description}</p>
      {action ? <div className="mt-1">{action}</div> : null}
    </EmptyState>
  )
}

/** 面板错误态：后端 message + 重试。 */
export function PanelError({ message, onRetry }: { message: string; onRetry: () => void }) {
  const { t } = useI18n()

  return (
    <div className="flex flex-col items-start gap-3">
      <Alert status="danger" className="w-full">
        <Alert.Indicator>
          <AlertIcon aria-hidden="true" className="size-4" />
        </Alert.Indicator>
        <Alert.Content>
          <Alert.Title>{t('integrations.shared.loadFailed')}</Alert.Title>
          <Alert.Description className="break-all">{message}</Alert.Description>
        </Alert.Content>
      </Alert>
      <Button variant="secondary" size="sm" className="h-10 gap-1.5 md:h-9" onPress={onRetry}>
        <RefreshIcon size={14} />
        {t('common.retry')}
      </Button>
    </div>
  )
}

export interface ConfirmDialogProps {
  open: boolean
  title: string
  description: string
  confirmLabel?: string
  cancelLabel?: string
  isPending?: boolean
  error?: string | null
  onOpenChange: (open: boolean) => void
  onConfirm: () => void
}

/** 删除 / 卸载等危险操作的确认弹窗（不可点外部关闭）。 */
export function ConfirmDialog({
  open,
  title,
  description,
  confirmLabel,
  cancelLabel,
  isPending = false,
  error = null,
  onOpenChange,
  onConfirm,
}: ConfirmDialogProps) {
  const { t } = useI18n()
  const confirmText = confirmLabel ?? t('common.delete')
  const cancelText = cancelLabel ?? t('common.cancel')

  return (
    <AlertDialog
      isOpen={open}
      onOpenChange={(next) => {
        if (isPending && !next) return
        onOpenChange(next)
      }}
    >
      <AlertDialog.Backdrop isDismissable={false}>
        <AlertDialog.Container placement="center">
          <AlertDialog.Dialog role="alertdialog" aria-label={title}>
            <AlertDialog.Header>
              <AlertDialog.Icon status="danger">
                <AlertIcon size={18} aria-hidden="true" />
              </AlertDialog.Icon>
              <AlertDialog.Heading>{title}</AlertDialog.Heading>
            </AlertDialog.Header>

            <AlertDialog.Body className="flex flex-col gap-2">
              <p className="text-[13px] leading-6 break-words text-muted">{description}</p>
              {error ? (
                <p role="alert" className="text-[12px] leading-5 break-all text-danger">
                  {error}
                </p>
              ) : null}
            </AlertDialog.Body>

            <AlertDialog.Footer>
              <Button
                variant="ghost"
                size="sm"
                className="h-10 md:h-9"
                isDisabled={isPending}
                onPress={() => onOpenChange(false)}
              >
                {cancelText}
              </Button>
              <Button
                variant="danger"
                size="sm"
                className="h-10 gap-1.5 md:h-9"
                isPending={isPending}
                onPress={onConfirm}
                autoFocus
              >
                <TrashIcon size={14} aria-hidden="true" />
                {confirmText}
              </Button>
            </AlertDialog.Footer>
          </AlertDialog.Dialog>
        </AlertDialog.Container>
      </AlertDialog.Backdrop>
    </AlertDialog>
  )
}

/** JSON 文本域解析结果。 */
export type JsonParseResult<T> = { ok: true; value: T } | { ok: false; error: string }

/**
 * 解析「JSON 对象」文本域：空文本视为 `{}`，数组 / 标量 / 语法错误都返回可展示的错误。
 * `label` 由调用方传入已翻译的字段名，`t` 用于拼错误文案。
 */
export function parseJsonObject(
  text: string,
  label: string,
  t: Translate,
): JsonParseResult<Record<string, unknown>> {
  const raw = text.trim()
  if (!raw) return { ok: true, value: {} }

  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return { ok: false, error: t('integrations.shared.jsonInvalid', { label }) }
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    return { ok: false, error: t('integrations.shared.jsonNotObject', { label }) }
  }
  return { ok: true, value: parsed as Record<string, unknown> }
}

/** 解析 `Record<string, string>`（env / headers）：键值都必须是非空键 + 字符串值。 */
export function parseJsonStringMap(
  text: string,
  label: string,
  t: Translate,
): JsonParseResult<Record<string, string>> {
  const object = parseJsonObject(text, label, t)
  if (!object.ok) return object

  const entries = Object.entries(object.value)
  for (const [key, value] of entries) {
    if (typeof value !== 'string') {
      return { ok: false, error: t('integrations.shared.jsonNotString', { label, key }) }
    }
  }
  return { ok: true, value: object.value as Record<string, string> }
}
