'use client'

/**
 * 模型页三块面板（用户列表 / 目录 / Provider 密钥 / 路由）共用的基础件：
 * 列表骨架、空态、错误态（后端 message + 重试）、危险操作确认弹窗。
 *
 * 与集成页的 `integrations/shared` 保持同样的交互与触控尺寸（≥40px），
 * 但文案走 `models.*` / `common.*`，避免跨模块耦合。
 */

import type { ReactNode } from 'react'
import { Alert, AlertDialog, Button, Chip, EmptyState, Skeleton, Tooltip } from '@heroui/react'
import { AlertIcon, RefreshIcon, TrashIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import { formatDateTime } from '@/lib/utils'

/** 列表骨架：默认 3 行，避免首屏跳动。 */
export function ModelsSkeleton({ rows = 3, label }: { rows?: number; label?: string }) {
  const { t } = useI18n()

  return (
    <div role="status" aria-live="polite" aria-busy="true" className="flex flex-col gap-3">
      <span className="sr-only">{label ?? t('models.skeleton')}</span>
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

/** 空态：图标 + 标题 + 说明 + 可选主操作。 */
export function ModelsEmpty({
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

/** 错误态：标题 + 后端 message + 重试。 */
export function ModelsError({
  title,
  message,
  onRetry,
}: {
  title: string
  message: string
  onRetry: () => void
}) {
  const { t } = useI18n()

  return (
    <div className="flex flex-col items-start gap-3">
      <Alert status="danger" className="w-full">
        <Alert.Indicator>
          <AlertIcon aria-hidden="true" className="size-4" />
        </Alert.Indicator>
        <Alert.Content>
          <Alert.Title>{title}</Alert.Title>
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

export interface ModelsHealthBadgeProps {
  /** 最近一次成功调用（来自路由健康字段，或探测后刷新）。 */
  lastOkAt?: string | null
  /** 最近一次健康探测时间。 */
  lastProbeAt?: string | null
  /** 最近一次失败原因；非空时优先展示为异常。 */
  lastError?: string
  className?: string
}

/**
 * 路由 / Provider 健康徽标：有 last_error 红色、有 last_ok_at 绿色、否则灰色「未探测」。
 * 悬停或键盘聚焦时用 tooltip 展示具体错误与探测时间。
 */
export function ModelsHealthBadge({
  lastOkAt,
  lastProbeAt,
  lastError,
  className,
}: ModelsHealthBadgeProps) {
  const { t, locale } = useI18n()

  const error = lastError?.trim() ?? ''
  const ok = Boolean(lastOkAt)
  const color = error ? 'danger' : ok ? 'success' : 'default'
  const label = error
    ? t('models.health.failed')
    : ok
      ? t('models.health.ok')
      : t('models.health.unknown')

  const details = [
    error ? `${t('models.health.errorLabel')}: ${error}` : null,
    ok ? t('models.health.lastOk', { time: formatDateTime(lastOkAt, locale) }) : null,
    lastProbeAt
      ? t('models.health.lastProbe', { time: formatDateTime(lastProbeAt, locale) })
      : t('models.health.neverProbed'),
  ].filter(Boolean)

  return (
    <Tooltip>
      <Tooltip.Trigger className="inline-flex" tabIndex={0} aria-label={details.join(' · ')}>
        <Chip size="sm" variant="soft" color={color} className={className}>
          <span aria-hidden="true">●</span>
          <span>{label}</span>
        </Chip>
      </Tooltip.Trigger>
      <Tooltip.Content>
        <span className="flex flex-col gap-0.5">
          {details.map((line) => (
            <span key={line}>{line}</span>
          ))}
        </span>
      </Tooltip.Content>
    </Tooltip>
  )
}

export interface ModelsConfirmDialogProps {  open: boolean
  title: string
  description: string
  isPending?: boolean
  error?: string | null
  onOpenChange: (open: boolean) => void
  onConfirm: () => void
}

/** 删除 / 停用等危险操作的确认弹窗（不可点外部关闭）。 */
export function ModelsConfirmDialog({
  open,
  title,
  description,
  isPending = false,
  error = null,
  onOpenChange,
  onConfirm,
}: ModelsConfirmDialogProps) {
  const { t } = useI18n()

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
                {t('common.cancel')}
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
                {t('common.delete')}
              </Button>
            </AlertDialog.Footer>
          </AlertDialog.Dialog>
        </AlertDialog.Container>
      </AlertDialog.Backdrop>
    </AlertDialog>
  )
}
