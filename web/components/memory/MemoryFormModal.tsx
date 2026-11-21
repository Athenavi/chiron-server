'use client'

/**
 * 新建 / 编辑记忆的弹窗表单。
 *
 * - 字段：槽位（Select）、内容（TextArea，必填）、作用域（Input，默认 global）、
 *   来源（Select）、置信度（0–1，可选）；编辑时作用域只读（后端 PATCH 不支持改 scope）；
 * - `summary` 必须绑定会话作用域：scope 不是 `session:` 开头时前端直接阻止并提示；
 * - 提交中 isPending，失败展示后端 message（表单一处 Alert + 字段内联 FieldError）；
 * - 错误通过 `FieldError` 关联到对应输入框（aria-invalid + aria-describedby 由
 *   react-aria 的 TextField 自动接线），`global` / `session:` 等技术标识保留原文。
 */

import { useEffect, useState } from 'react'
import {
  Alert,
  Button,
  Description,
  FieldError,
  Input,
  Label,
  ListBox,
  Modal,
  Select,
  TextArea,
  TextField,
} from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError, type Memory, type MemorySlot } from '@/lib/types'
import { AlertIcon, SparkIcon } from '@/components/ui-bridge/icons'
import { useI18n } from '@/lib/i18n'
import {
  GLOBAL_SCOPE,
  SLOT_ORDER,
  isSessionScope,
  slotDescription,
  slotLabel,
  sourceOptions,
} from './memory-meta'

export type MemorySaveMode = 'create' | 'update'

export interface MemoryFormModalProps {
  open: boolean
  /** null = 新建；否则编辑该条记忆。 */
  memory: Memory | null
  onOpenChange: (open: boolean) => void
  onSaved: (memory: Memory, mode: MemorySaveMode) => void
}

const DEFAULT_SLOT: MemorySlot = 'facts'
const DEFAULT_SOURCE = 'user'
const SCOPE_PATTERN = /^(global|user|session:.+)$/

interface FormErrors {
  content: string | null
  scope: string | null
  confidence: string | null
}

const EMPTY_ERRORS: FormErrors = { content: null, scope: null, confidence: null }

export function MemoryFormModal({ open, memory, onOpenChange, onSaved }: MemoryFormModalProps) {
  const { t } = useI18n()
  const editing = Boolean(memory)

  const [slot, setSlot] = useState<MemorySlot>(DEFAULT_SLOT)
  const [content, setContent] = useState('')
  const [scope, setScope] = useState(GLOBAL_SCOPE)
  const [source, setSource] = useState(DEFAULT_SOURCE)
  const [confidence, setConfidence] = useState('')
  const [errors, setErrors] = useState<FormErrors>(EMPTY_ERRORS)
  const [formError, setFormError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  // 每次打开都按当前目标重置表单（编辑 / 新建共用一套状态）
  useEffect(() => {
    if (!open) return
    setSlot(memory?.slot ?? DEFAULT_SLOT)
    setContent(memory?.content ?? '')
    setScope(memory?.scope ?? GLOBAL_SCOPE)
    setSource(memory?.source ?? DEFAULT_SOURCE)
    setConfidence(memory ? String(memory.confidence) : '')
    setErrors(EMPTY_ERRORS)
    setFormError(null)
    setSubmitting(false)
  }, [open, memory])

  const summaryNeedsSession = slot === 'summary' && !isSessionScope(scope.trim())
  /** 编辑时作用域不可改：非会话作用域下不提供 summary，避免选了也提交不了。 */
  const slotOptions =
    editing && !isSessionScope(scope.trim())
      ? SLOT_ORDER.filter((value) => value !== 'summary')
      : SLOT_ORDER
  const sourceChoices = sourceOptions(t)

  useEffect(() => {
    if (!summaryNeedsSession) setErrors((prev) => (prev.scope ? { ...prev, scope: null } : prev))
  }, [summaryNeedsSession])

  async function submit() {
    if (submitting) return

    const trimmed = content.trim()
    const scopeValue = scope.trim() || GLOBAL_SCOPE
    const nextErrors: FormErrors = { content: null, scope: null, confidence: null }

    if (!trimmed) nextErrors.content = t('memory.form.errorContent')
    if (slot === 'summary' && !isSessionScope(scopeValue)) {
      nextErrors.scope = t('memory.form.errorScopeSummary')
    } else if (!editing && !SCOPE_PATTERN.test(scopeValue)) {
      nextErrors.scope = t('memory.form.errorScope')
    }

    let confidenceValue: number | undefined
    const confidenceText = confidence.trim()
    if (confidenceText) {
      const parsed = Number(confidenceText)
      if (!Number.isFinite(parsed) || parsed < 0 || parsed > 1) {
        nextErrors.confidence = t('memory.form.errorConfidence')
      } else {
        confidenceValue = parsed
      }
    }

    setErrors(nextErrors)
    setFormError(null)
    if (nextErrors.content || nextErrors.scope || nextErrors.confidence) return

    setSubmitting(true)
    try {
      const saved = memory
        ? await api.updateMemory(memory.id, {
            content: trimmed,
            slot,
            source,
            confidence: confidenceValue,
          })
        : await api.createMemory({
            slot,
            content: trimmed,
            scope: scopeValue,
            source,
            confidence: confidenceValue,
          })
      onSaved(saved, memory ? 'update' : 'create')
    } catch (cause) {
      setFormError(ApiError.from(cause, t('memory.error.save')).message)
    } finally {
      setSubmitting(false)
    }
  }

  function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    void submit()
  }

  const title = editing ? t('memory.form.editTitle') : t('memory.form.createTitle')

  return (
    <Modal
      isOpen={open}
      onOpenChange={(next) => {
        if (submitting && !next) return
        onOpenChange(next)
      }}
    >
      <Modal.Backdrop>
        <Modal.Container placement="center" scroll="inside">
          <Modal.Dialog aria-label={title} className="w-full max-w-lg">
            <Modal.Header>
              <Modal.Icon className="bg-accent-soft text-accent">
                <SparkIcon size={18} />
              </Modal.Icon>
              <div className="min-w-0 flex-1">
                <Modal.Heading>{title}</Modal.Heading>
                <p className="mt-0.5 text-xs leading-5 text-muted">{t('memory.form.hint')}</p>
              </div>
            </Modal.Header>

            <Modal.Body className="flex flex-col gap-4">
              {formError ? (
                <Alert status="danger">
                  <Alert.Indicator>
                    <AlertIcon size={16} aria-hidden="true" />
                  </Alert.Indicator>
                  <Alert.Content>
                    <Alert.Title>{t('memory.form.errorTitle')}</Alert.Title>
                    <Alert.Description>{formError}</Alert.Description>
                  </Alert.Content>
                </Alert>
              ) : null}

              <form onSubmit={handleSubmit} noValidate className="flex flex-col gap-4">
                <Select
                  fullWidth
                  isDisabled={submitting}
                  selectedKey={slot}
                  onSelectionChange={(key) => {
                    if (key !== null) setSlot(String(key) as MemorySlot)
                  }}
                >
                  <Label>{t('memory.form.slot')}</Label>
                  <Select.Trigger>
                    <Select.Value />
                    <Select.Indicator />
                  </Select.Trigger>
                  <Select.Popover>
                    <ListBox>
                      {slotOptions.map((value) => (
                        <ListBox.Item
                          key={value}
                          id={value}
                          textValue={slotLabel(value, t)}
                        >
                          <span className="flex min-w-0 flex-col">
                            <span>{slotLabel(value, t)}</span>
                            <span className="truncate text-[11px] text-muted">
                              {slotDescription(value, t)}
                            </span>
                          </span>
                          <ListBox.ItemIndicator />
                        </ListBox.Item>
                      ))}
                    </ListBox>
                  </Select.Popover>
                </Select>

                <TextField
                  fullWidth
                  isDisabled={submitting}
                  isInvalid={Boolean(errors.content)}
                >
                  <Label>{t('memory.form.content')}</Label>
                  <TextArea
                    autoFocus
                    rows={5}
                    value={content}
                    placeholder={t('memory.form.contentPlaceholder')}
                    onChange={(event) => setContent(event.target.value)}
                    className="min-h-[120px] resize-y text-[13px]"
                  />
                  {errors.content ? (
                    <FieldError className="text-danger">{errors.content}</FieldError>
                  ) : (
                    <Description>{t('memory.form.contentDescription')}</Description>
                  )}
                </TextField>

                <TextField
                  fullWidth
                  isDisabled={submitting || editing}
                  isInvalid={Boolean(errors.scope)}
                  value={scope}
                  onChange={(value) => setScope(value)}
                >
                  <Label>{t('memory.form.scope')}</Label>
                  <Input
                    autoComplete="off"
                    spellCheck={false}
                    placeholder={GLOBAL_SCOPE}
                    className="font-mono text-[13px]"
                  />
                  {errors.scope ? (
                    <FieldError className="text-danger">{errors.scope}</FieldError>
                  ) : editing ? (
                    <Description>{t('memory.form.scopeDescriptionEdit')}</Description>
                  ) : summaryNeedsSession ? (
                    <Description className="text-warning">
                      {t('memory.form.scopeDescriptionSummary')}
                    </Description>
                  ) : (
                    <Description>{t('memory.form.scopeDescription')}</Description>
                  )}
                </TextField>

                <div className="grid gap-4 sm:grid-cols-2">
                  <Select
                    fullWidth
                    isDisabled={submitting}
                    selectedKey={source}
                    onSelectionChange={(key) => {
                      if (key !== null) setSource(String(key))
                    }}
                  >
                    <Label>{t('memory.form.source')}</Label>
                    <Select.Trigger>
                      <Select.Value />
                      <Select.Indicator />
                    </Select.Trigger>
                    <Select.Popover>
                      <ListBox>
                        {sourceChoices.map((option) => (
                          <ListBox.Item
                            key={option.id}
                            id={option.id}
                            textValue={option.label}
                          >
                            {option.label}
                            <ListBox.ItemIndicator />
                          </ListBox.Item>
                        ))}
                      </ListBox>
                    </Select.Popover>
                  </Select>

                  <TextField
                    fullWidth
                    isDisabled={submitting}
                    isInvalid={Boolean(errors.confidence)}
                    value={confidence}
                    onChange={(value) => setConfidence(value)}
                  >
                    <Label>{t('memory.form.confidence')}</Label>
                    <Input
                      inputMode="decimal"
                      autoComplete="off"
                      placeholder={t('memory.form.confidencePlaceholder')}
                      className="tabular"
                    />
                    {errors.confidence ? (
                      <FieldError className="text-danger">{errors.confidence}</FieldError>
                    ) : (
                      <Description>{t('memory.form.confidenceDescription')}</Description>
                    )}
                  </TextField>
                </div>
              </form>
            </Modal.Body>

            <Modal.Footer>
              <Button
                variant="ghost"
                className="h-10 px-4"
                isDisabled={submitting}
                onPress={() => onOpenChange(false)}
              >
                {t('memory.form.cancel')}
              </Button>
              <Button
                variant="primary"
                className="h-10 px-4"
                isPending={submitting}
                onPress={() => void submit()}
              >
                {editing ? t('memory.form.submitUpdate') : t('memory.form.submitCreate')}
              </Button>
            </Modal.Footer>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  )
}
