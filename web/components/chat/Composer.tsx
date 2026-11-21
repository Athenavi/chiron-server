'use client'

/**
 * 消息输入器。
 *
 * - HeroUI TextArea 自动增高（1–6 行，超出滚动）；
 * - Enter 发送 / Shift+Enter 换行；macOS Cmd+Enter、Windows Ctrl+Enter 也发送；
 * - 输入法组合期（IME）不触发发送；
 * - 空闲时「发送」；运行中主按钮变成「插话」（`onSteer`），旁边单独一个停止按钮；
 * - 草稿只在 `onSend` / `onSteer` resolve 之后清空，失败时保留文本并 toast；
 * - 附件按钮是占位（disabled + Tooltip），不做假上传。
 */

import { useEffect, useRef, useState } from 'react'
import { Button, Kbd, TextArea, Tooltip, toast } from '@heroui/react'
import { ApiError } from '@/lib/types'
import { useI18n } from '@/lib/i18n'
import { PaperclipIcon, SendIcon, StopIcon } from '@/components/ui-bridge/icons'
import { cn } from '@/lib/utils'

const MIN_ROWS = 1
const MAX_ROWS = 6

export interface ComposerProps {
  running: boolean
  disabled?: boolean
  placeholder?: string
  onSend: (text: string) => Promise<void> | void
  onSteer: (text: string) => Promise<void> | void
  onStop: () => void
}

export function Composer({
  running,
  disabled = false,
  placeholder,
  onSend,
  onSteer,
  onStop,
}: ComposerProps) {
  const { t } = useI18n()
  const [text, setText] = useState('')
  const [pending, setPending] = useState(false)
  const textareaRef = useRef<HTMLTextAreaElement | null>(null)
  const composingRef = useRef(false)
  // 同步闸门：连续按 Enter 时 state 还没重渲染，用 ref 保证只提交一次
  const submittingRef = useRef(false)

  const canSubmit = !disabled && !pending && text.trim().length > 0
  // 提交中不禁用输入框：禁用会丢焦点、收起软键盘，也挡不住重复提交
  // （真正的防抖由 canSubmit 里的 !pending 负责）。
  const blocked = disabled

  // 依据内容自动增高：先归零再读 scrollHeight，超过 6 行改为内部滚动
  useEffect(() => {
    const element = textareaRef.current
    if (!element) return
    element.style.height = 'auto'
    const styles = window.getComputedStyle(element)
    const lineHeight = Number.parseFloat(styles.lineHeight) || 22
    const padding = Number.parseFloat(styles.paddingTop) + Number.parseFloat(styles.paddingBottom)
    const min = lineHeight * MIN_ROWS + padding
    const max = lineHeight * MAX_ROWS + padding
    const next = Math.max(min, Math.min(element.scrollHeight, max))
    element.style.height = `${next}px`
    element.style.overflowY = element.scrollHeight > max ? 'auto' : 'hidden'
  }, [text])

  async function submit() {
    if (submittingRef.current || !canSubmit) return
    const value = text.trim()
    if (!value) return
    submittingRef.current = true
    setPending(true)
    try {
      if (running) await onSteer(value)
      else await onSend(value)
      // 只有成功才清空：失败时保留草稿
      setText('')
    } catch (cause) {
      toast.danger(
        ApiError.from(cause, running ? t('chat.composer.steerFailed') : t('chat.composer.sendFailed'))
          .message,
      )
    } finally {
      submittingRef.current = false
      setPending(false)
    }
  }

  function handleKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>) {
    // IME 组合期（尤其 Safari 提交输入法的 Enter）不能当发送：
    // composingRef 覆盖组合过程，isComposing/keyCode 229 兜住 Safari 的事件顺序差异。
    if (composingRef.current || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) {
      return
    }
    if (event.key !== 'Enter') return
    // Shift+Enter 换行
    if (event.shiftKey) return
    // Enter / Cmd+Enter / Ctrl+Enter 发送
    event.preventDefault()
    void submit()
  }

  return (
    <div className="chat-shell chat-composer shrink-0 pt-1 pb-[calc(0.75rem+env(safe-area-inset-bottom))] md:pb-4">
      <div
        className={cn(
          'rounded-2xl border border-border bg-surface px-2 py-1.5 shadow-sm transition-colors',
          'focus-within:border-accent/60 focus-within:ring-2 focus-within:ring-accent/15',
        )}
      >
        <TextArea
          ref={textareaRef}
          rows={1}
          fullWidth
          aria-label={t('chat.composer.inputAria')}
          value={text}
          disabled={blocked}
          placeholder={placeholder ?? t('chat.composer.placeholder')}
          onChange={(event) => setText(event.target.value)}
          onKeyDown={handleKeyDown}
          onCompositionStart={() => {
            composingRef.current = true
          }}
          onCompositionEnd={() => {
            composingRef.current = false
          }}
          className={cn(
            'min-h-9 max-h-[168px] w-full resize-none rounded-lg border-0 bg-transparent px-2 py-1.5',
            'text-sm leading-6 text-foreground shadow-none outline-none',
            'placeholder:text-subtle focus:bg-transparent focus:ring-0',
          )}
        />

        <div className="mt-0.5 flex items-end justify-between gap-2 px-0.5">
          <div className="flex min-w-0 items-center gap-1.5">
            <Tooltip>
              <Tooltip.Trigger className="inline-flex">
                <Button
                  variant="ghost"
                  isIconOnly
                  isDisabled
                  aria-label={t('chat.composer.attach')}
                  className="h-10 w-10 shrink-0 rounded-md text-muted md:h-8 md:w-8"
                >
                  <PaperclipIcon size={16} />
                </Button>
              </Tooltip.Trigger>
              <Tooltip.Content>{t('chat.composer.comingSoon')}</Tooltip.Content>
            </Tooltip>

            <span className="hidden min-w-0 items-center gap-1 text-[11px] text-subtle sm:flex">
              <Kbd>Enter</Kbd>
              <span>{running ? t('chat.composer.enterSteer') : t('chat.composer.enterSend')}</span>
              <span aria-hidden="true" className="px-0.5">
                ·
              </span>
              <Kbd>Shift</Kbd>
              <span aria-hidden="true">+</span>
              <Kbd>Enter</Kbd>
              <span>{t('chat.composer.newline')}</span>
            </span>
          </div>

          <div className="flex shrink-0 items-center gap-1.5">
            {running ? (
              <>
                <Button
                  variant="primary"
                  size="sm"
                  isPending={pending}
                  isDisabled={!canSubmit}
                  onPress={() => void submit()}
                  aria-label={t('chat.composer.steerAria')}
                  className="h-10 gap-1.5 rounded-full px-3 md:h-9"
                >
                  <SendIcon size={15} />
                  {t('chat.composer.steer')}
                </Button>
                <Tooltip>
                  <Tooltip.Trigger className="inline-flex">
                    <Button
                      variant="danger-soft"
                      isIconOnly
                      onPress={onStop}
                      aria-label={t('chat.composer.stop')}
                      className="h-10 w-10 rounded-full md:h-9 md:w-9"
                    >
                      <StopIcon size={14} />
                    </Button>
                  </Tooltip.Trigger>
                  <Tooltip.Content>{t('chat.composer.stop')}</Tooltip.Content>
                </Tooltip>
              </>
            ) : (
              <Button
                variant="primary"
                isIconOnly
                isPending={pending}
                isDisabled={!canSubmit}
                onPress={() => void submit()}
                aria-label={t('chat.composer.send')}
                className="h-10 w-10 rounded-full md:h-9 md:w-9"
              >
                <SendIcon size={16} />
              </Button>
            )}
          </div>
        </div>
      </div>

      <p className="mt-1.5 text-center text-[11px] leading-5 text-subtle">
        {running ? t('chat.composer.hintRunning') : t('chat.composer.hintIdle')}
      </p>
    </div>
  )
}

export default Composer
