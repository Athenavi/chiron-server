'use client'

/**
 * 审批弹窗：一次只展示队首的审批，支持备注、允许 / 拒绝与过期倒计时。
 *
 * - 弹窗不可点击外部关闭（审批必须显式决策），焦点管理交给 HeroUI Modal；
 * - 过期（`expires_at`）后按钮禁用，并由 `useChatStream` 的定时器自动出队；
 * - 参数 JSON 美化后限高滚动，避免撑破弹窗。
 */

import { useEffect, useId, useMemo, useState } from 'react'
import { Button, Chip, Modal } from '@heroui/react'
import { useI18n } from '@/lib/i18n'
import { AlertIcon, CheckIcon, WrenchIcon, XIcon } from '@/components/ui-bridge/icons'
import type { ApprovalDecision } from '@/lib/types'
import type { ApprovalView } from './types'

function prettyValue(value: unknown): string {
  if (value === undefined || value === null) return ''
  if (typeof value === 'string') return value
  try {
    return JSON.stringify(value, null, 2)
  } catch {
    return String(value)
  }
}

export interface ApprovalDialogProps {
  approval: ApprovalView | null
  deciding: boolean
  /** 队列中其余待审批数量（用于提示）。 */
  pendingCount?: number
  onDecide: (approvalId: string, decision: ApprovalDecision, note?: string) => Promise<void> | void
}

export function ApprovalDialog({ approval, deciding, pendingCount = 0, onDecide }: ApprovalDialogProps) {
  const { t } = useI18n()
  const descriptionId = useId()
  const [note, setNote] = useState('')
  const [remaining, setRemaining] = useState<number | null>(null)

  const approvalId = approval?.approval_id ?? null
  const expiresAt = approval?.expires_at ?? null

  // 切换审批时清空备注
  useEffect(() => {
    setNote('')
  }, [approvalId])

  // 倒计时（无 expires_at 时不显示）
  useEffect(() => {
    if (!expiresAt) {
      setRemaining(null)
      return
    }
    const update = () => {
      const target = Date.parse(expiresAt)
      setRemaining(Number.isNaN(target) ? null : Math.max(0, target - Date.now()))
    }
    update()
    const timer = window.setInterval(update, 1000)
    return () => window.clearInterval(timer)
  }, [expiresAt])

  const expired = remaining !== null && remaining <= 0
  const seconds = remaining === null ? null : Math.ceil(remaining / 1000)
  const argsText = useMemo(() => prettyValue(approval?.args), [approval?.args])
  const busy = deciding

  async function submit(decision: ApprovalDecision) {
    if (!approvalId || expired) return
    const trimmed = note.trim()
    try {
      await onDecide(approvalId, decision, trimmed ? trimmed : undefined)
    } catch {
      // 失败提示由 useChatStream 统一 toast，这里只保证不产生未处理的 Promise 拒绝
    }
  }

  return (
    <Modal.Backdrop isOpen={Boolean(approval)} isDismissable={false} isKeyboardDismissDisabled>
      <Modal.Container placement="center" scroll="inside">
        <Modal.Dialog
          role="alertdialog"
          aria-label={t('chat.approval.dialogAria')}
          aria-describedby={descriptionId}
          aria-busy={busy}
        >
          <Modal.Header>
            <Modal.Icon className="bg-warning-soft text-warning">
              <AlertIcon size={18} />
            </Modal.Icon>
            <div className="min-w-0 flex-1">
              <Modal.Heading>{t('chat.approval.title')}</Modal.Heading>
              <p id={descriptionId} className="mt-0.5 text-xs text-muted">
                {t('chat.approval.description')}
              </p>
            </div>
            {seconds !== null ? (
              <Chip size="sm" variant="soft" color={expired ? 'danger' : 'warning'} className="shrink-0">
                {expired ? t('chat.approval.expired') : t('chat.approval.expiresIn', { seconds })}
              </Chip>
            ) : null}
          </Modal.Header>

          <Modal.Body className="flex flex-col gap-3">
            <div className="flex items-center gap-2">
              <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md border border-border bg-surface text-muted">
                <WrenchIcon size={14} />
              </span>
              <div className="min-w-0 flex-1">
                <p className="truncate font-mono text-sm text-foreground">
                  {approval?.name || t('chat.tool.fallbackName')}
                </p>
                {approval?.tool_call_id ? (
                  <p className="truncate font-mono text-[11px] text-subtle">{approval.tool_call_id}</p>
                ) : null}
              </div>
              {pendingCount > 0 ? (
                <Chip size="sm" variant="soft" color="default" className="shrink-0">
                  {t('chat.approval.pending', { n: pendingCount })}
                </Chip>
              ) : null}
            </div>

            {argsText ? (
              <div>
                <p className="mb-1 text-[11px] font-medium uppercase tracking-wide text-subtle">
                  {t('chat.tool.args')}
                </p>
                <pre className="max-h-64 overflow-auto rounded-md border border-border bg-surface px-3 py-2 font-mono text-[12px] leading-relaxed text-foreground">
                  <code>{argsText}</code>
                </pre>
              </div>
            ) : null}

            <div>
              <label htmlFor="approval-note" className="mb-1 block text-[11px] font-medium uppercase tracking-wide text-subtle">
                {t('chat.approval.noteLabel')}
              </label>
              <textarea
                id="approval-note"
                value={note}
                onChange={(event) => setNote(event.target.value)}
                rows={2}
                disabled={expired || busy}
                placeholder={t('chat.approval.notePlaceholder')}
                className="w-full resize-none rounded-md border border-border bg-surface px-2.5 py-2 text-sm text-foreground outline-none placeholder:text-subtle disabled:cursor-not-allowed disabled:opacity-60"
              />
            </div>
          </Modal.Body>

          <Modal.Footer>
            <Button
              variant="danger-soft"
              isDisabled={expired || busy || !approvalId}
              onPress={() => void submit('deny')}
              autoFocus
            >
              <XIcon size={14} />
              {t('chat.approval.deny')}
            </Button>
            <Button
              variant="primary"
              isPending={busy}
              isDisabled={expired || !approvalId}
              onPress={() => void submit('allow')}
            >
              <CheckIcon size={14} />
              {t('chat.approval.allow')}
            </Button>
          </Modal.Footer>
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  )
}
