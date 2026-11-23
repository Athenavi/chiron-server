'use client'

import { useEffect, useState } from 'react'
import { Button, Modal, toast } from '@heroui/react'
import { api } from '@/lib/api'
import {
  DEFAULT_CONVERSATION_SETTINGS,
  type ChatSession,
  type ConversationSettings,
  type ReplyLanguage,
  type ReplyStyle,
  type ReasoningEffort,
  type ThinkingLanguage,
  type ToolAuthorization,
} from '@/lib/types'
import { useI18n } from '@/lib/i18n'
import { SlidersIcon } from '@/components/ui-bridge/icons'
import { ChoiceGroup, type Choice } from './DisplaySettingsDialog'

export interface ConversationSettingsDialogProps {
  session: ChatSession | null
  isOpen: boolean
  onOpenChange: (open: boolean) => void
  onSaved: (settings: ConversationSettings) => void
}

export function ConversationSettingsDialog({
  session,
  isOpen,
  onOpenChange,
  onSaved,
}: ConversationSettingsDialogProps) {
  const { t } = useI18n()
  const [settings, setSettings] = useState<ConversationSettings>(DEFAULT_CONVERSATION_SETTINGS)
  const [saving, setSaving] = useState(false)

  useEffect(() => {
    if (!isOpen) return
    setSettings({ ...DEFAULT_CONVERSATION_SETTINGS, ...(session?.metadata?.chat ?? {}) })
  }, [isOpen, session?.id, session?.metadata?.chat])

  const toolOptions: ReadonlyArray<Choice<ToolAuthorization>> = [
    { value: 'ask', labelKey: 'chat.conversation.toolAsk' },
    { value: 'auto', labelKey: 'chat.conversation.toolAuto' },
    { value: 'readonly', labelKey: 'chat.conversation.toolReadonly' },
  ]
  const styleOptions: ReadonlyArray<Choice<ReplyStyle>> = [
    { value: 'concise', labelKey: 'chat.conversation.styleConcise' },
    { value: 'balanced', labelKey: 'chat.conversation.styleBalanced' },
    { value: 'detailed', labelKey: 'chat.conversation.styleDetailed' },
  ]
  const languageOptions: ReadonlyArray<Choice<ReplyLanguage>> = [
    { value: 'auto', labelKey: 'chat.conversation.languageAuto' },
    { value: 'zh', labelKey: 'chat.conversation.languageZh' },
    { value: 'en', labelKey: 'chat.conversation.languageEn' },
  ]
  const thinkingOptions: ReadonlyArray<Choice<ThinkingLanguage>> = [
    { value: 'auto', labelKey: 'chat.conversation.languageAuto' },
    { value: 'zh', labelKey: 'chat.conversation.languageZh' },
    { value: 'en', labelKey: 'chat.conversation.languageEn' },
  ]
  const effortOptions: ReadonlyArray<Choice<ReasoningEffort>> = [
    { value: 'auto', labelKey: 'chat.conversation.effortAuto' },
    { value: 'low', labelKey: 'chat.conversation.effortLow' },
    { value: 'medium', labelKey: 'chat.conversation.effortMedium' },
    { value: 'high', labelKey: 'chat.conversation.effortHigh' },
  ]

  async function save() {
    if (!session || saving) return
    setSaving(true)
    try {
      await api.updateSession(session.id, { settings })
      onSaved(settings)
      toast.success(t('chat.conversation.saved'))
      onOpenChange(false)
    } catch (cause) {
      toast.danger(cause instanceof Error ? cause.message : t('chat.conversation.saveFailed'))
    } finally {
      setSaving(false)
    }
  }

  return (
    <Modal isOpen={isOpen} onOpenChange={onOpenChange}>
      <Modal.Backdrop>
        <Modal.Container placement="center" scroll="inside">
          <Modal.Dialog aria-label={t('chat.conversation.title')} className="w-full max-w-md">
            <Modal.Header>
              <Modal.Icon className="bg-accent-soft text-accent">
                <SlidersIcon size={16} />
              </Modal.Icon>
              <div className="min-w-0 flex-1">
                <Modal.Heading>{t('chat.conversation.title')}</Modal.Heading>
              </div>
            </Modal.Header>
            <Modal.Body>
              <p className="mb-1 text-[12px] leading-5 text-muted">
                {t('chat.conversation.description')}
              </p>
              <ChoiceGroup
                label={t('chat.conversation.toolAuthorization')}
                value={settings.tool_authorization}
                options={toolOptions}
                onChange={(tool_authorization) => setSettings((s) => ({ ...s, tool_authorization }))}
              />
              <ChoiceGroup
                label={t('chat.conversation.replyStyle')}
                value={settings.reply_style}
                options={styleOptions}
                onChange={(reply_style) => setSettings((s) => ({ ...s, reply_style }))}
              />
              <ChoiceGroup
                label={t('chat.conversation.replyLanguage')}
                value={settings.reply_language}
                options={languageOptions}
                onChange={(reply_language) => setSettings((s) => ({ ...s, reply_language }))}
              />
              <ChoiceGroup
                label={t('chat.conversation.thinkingLanguage')}
                value={settings.thinking_language}
                options={thinkingOptions}
                onChange={(thinking_language) => setSettings((s) => ({ ...s, thinking_language }))}
              />
              <ChoiceGroup
                label={t('chat.conversation.reasoningEffort')}
                value={settings.reasoning_effort}
                options={effortOptions}
                onChange={(reasoning_effort) => setSettings((s) => ({ ...s, reasoning_effort }))}
              />
            </Modal.Body>
            <Modal.Footer>
              <Button variant="ghost" onPress={() => onOpenChange(false)}>
                {t('chat.conversation.cancel')}
              </Button>
              <Button variant="primary" isDisabled={saving} onPress={() => void save()}>
                {saving ? t('chat.conversation.saving') : t('chat.conversation.save')}
              </Button>
            </Modal.Footer>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  )
}

export default ConversationSettingsDialog
