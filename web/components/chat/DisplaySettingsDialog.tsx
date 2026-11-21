'use client'

/**
 * 对话显示设置弹窗。
 *
 * 每项是一组互斥选项（radiogroup）；改动即时生效并写入 localStorage，
 * 「恢复默认」只重置偏好，不关闭弹窗。
 */

import { Button, Modal } from '@heroui/react'
import { useI18n } from '@/lib/i18n'
import { SlidersIcon } from '@/components/ui-bridge/icons'
import {
  useDisplaySettings,
  type DisplayDensity,
  type DisplayFontSize,
  type DisplayLineHeight,
  type DisplayMessageGap,
  type DisplayWidth,
} from './DisplaySettings'

interface Choice<T extends string> {
  value: T
  labelKey: string
}

function ChoiceGroup<T extends string>({
  label,
  value,
  options,
  onChange,
}: {
  label: string
  value: T
  options: ReadonlyArray<Choice<T>>
  onChange: (value: T) => void
}) {
  const { t } = useI18n()
  return (
    <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border py-3 last:border-b-0">
      <span className="text-[13px] font-medium text-foreground">{label}</span>
      <div
        role="radiogroup"
        aria-label={label}
        className="flex shrink-0 rounded-lg border border-border bg-surface-secondary p-0.5"
      >
        {options.map((option) => {
          const selected = option.value === value
          return (
            <button
              key={option.value}
              type="button"
              role="radio"
              aria-checked={selected}
              onClick={() => onChange(option.value)}
              className={[
                'h-8 rounded-md px-2.5 text-[12px] transition-colors',
                'focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring',
                selected
                  ? 'bg-surface font-medium text-foreground shadow-xs'
                  : 'text-muted hover:text-foreground',
              ].join(' ')}
            >
              {t(option.labelKey)}
            </button>
          )
        })}
      </div>
    </div>
  )
}

export interface DisplaySettingsDialogProps {
  isOpen: boolean
  onOpenChange: (open: boolean) => void
}

export function DisplaySettingsDialog({ isOpen, onOpenChange }: DisplaySettingsDialogProps) {
  const { t } = useI18n()
  const { settings, update, reset } = useDisplaySettings()

  const densityOptions: ReadonlyArray<Choice<DisplayDensity>> = [
    { value: 'compact', labelKey: 'chat.display.densityCompact' },
    { value: 'standard', labelKey: 'chat.display.densityStandard' },
    { value: 'comfortable', labelKey: 'chat.display.densityComfortable' },
  ]
  const soundOptions: ReadonlyArray<Choice<'off' | 'on'>> = [
    { value: 'off', labelKey: 'chat.display.soundOff' },
    { value: 'on', labelKey: 'chat.display.soundOn' },
  ]
  const fontSizeOptions: ReadonlyArray<Choice<DisplayFontSize>> = [
    { value: 'compact', labelKey: 'chat.display.fontSizeCompact' },
    { value: 'standard', labelKey: 'chat.display.fontSizeStandard' },
    { value: 'large', labelKey: 'chat.display.fontSizeLarge' },
  ]
  const lineHeightOptions: ReadonlyArray<Choice<DisplayLineHeight>> = [
    { value: 'tight', labelKey: 'chat.display.lineHeightTight' },
    { value: 'standard', labelKey: 'chat.display.lineHeightStandard' },
    { value: 'loose', labelKey: 'chat.display.lineHeightLoose' },
  ]
  const gapOptions: ReadonlyArray<Choice<DisplayMessageGap>> = [
    { value: 'compact', labelKey: 'chat.display.gapCompact' },
    { value: 'standard', labelKey: 'chat.display.gapStandard' },
    { value: 'loose', labelKey: 'chat.display.gapLoose' },
  ]
  const widthOptions: ReadonlyArray<Choice<DisplayWidth>> = [
    { value: 'comfortable', labelKey: 'chat.display.widthComfortable' },
    { value: 'wide', labelKey: 'chat.display.widthWide' },
    { value: 'ultra', labelKey: 'chat.display.widthUltra' },
  ]

  return (
    <Modal isOpen={isOpen} onOpenChange={onOpenChange}>
      <Modal.Backdrop>
        <Modal.Container placement="center" scroll="inside">
          <Modal.Dialog aria-label={t('chat.display.title')} className="w-full max-w-md">
            <Modal.Header>
              <Modal.Icon className="bg-accent-soft text-accent">
                <SlidersIcon size={16} />
              </Modal.Icon>
              <div className="min-w-0 flex-1">
                <Modal.Heading>{t('chat.display.title')}</Modal.Heading>
              </div>
            </Modal.Header>
            <Modal.Body>
              <p className="mb-1 text-[12px] leading-5 text-muted">
                {t('chat.display.description')}
              </p>
              <ChoiceGroup
                label={t('chat.display.density')}
                value={settings.density}
                options={densityOptions}
                onChange={(density) => update({ density })}
              />
              <ChoiceGroup
                label={t('chat.display.sound')}
                value={settings.sound ? 'on' : 'off'}
                options={soundOptions}
                onChange={(sound) => update({ sound: sound === 'on' })}
              />
              <ChoiceGroup
                label={t('chat.display.fontSize')}
                value={settings.fontSize}
                options={fontSizeOptions}
                onChange={(fontSize) => update({ fontSize })}
              />
              <ChoiceGroup
                label={t('chat.display.lineHeight')}
                value={settings.lineHeight}
                options={lineHeightOptions}
                onChange={(lineHeight) => update({ lineHeight })}
              />
              <ChoiceGroup
                label={t('chat.display.messageGap')}
                value={settings.messageGap}
                options={gapOptions}
                onChange={(messageGap) => update({ messageGap })}
              />
              <ChoiceGroup
                label={t('chat.display.width')}
                value={settings.width}
                options={widthOptions}
                onChange={(width) => update({ width })}
              />
            </Modal.Body>
            <Modal.Footer>
              <Button variant="ghost" onPress={reset}>
                {t('chat.display.reset')}
              </Button>
              <Button variant="primary" onPress={() => onOpenChange(false)}>
                {t('chat.display.done')}
              </Button>
            </Modal.Footer>
          </Modal.Dialog>
        </Modal.Container>
      </Modal.Backdrop>
    </Modal>
  )
}

export default DisplaySettingsDialog
