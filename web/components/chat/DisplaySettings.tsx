'use client'

/**
 * 对话显示偏好：界面密度 / 音效 / 字号 / 行距 / 消息间距 / 内容宽度。
 *
 * 只影响聊天页阅读体验，保存在本机 localStorage；Provider 负责跨组件共享，
 * `displayVars` 把偏好映射成聊天壳的 CSS 变量，密度走 `data-density`。
 */

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type CSSProperties,
  type ReactNode,
} from 'react'

export type DisplayDensity = 'compact' | 'standard' | 'comfortable'
export type DisplayFontSize = 'compact' | 'standard' | 'large'
export type DisplayLineHeight = 'tight' | 'standard' | 'loose'
export type DisplayMessageGap = 'compact' | 'standard' | 'loose'
export type DisplayWidth = 'comfortable' | 'wide' | 'ultra'

export interface DisplaySettings {
  density: DisplayDensity
  sound: boolean
  fontSize: DisplayFontSize
  lineHeight: DisplayLineHeight
  messageGap: DisplayMessageGap
  width: DisplayWidth
}

export const DEFAULT_DISPLAY_SETTINGS: DisplaySettings = {
  density: 'standard',
  sound: false,
  fontSize: 'standard',
  lineHeight: 'standard',
  messageGap: 'standard',
  width: 'comfortable',
}

const STORAGE_KEY = 'chiron.chat.display'

const WIDTH_VALUES: Record<DisplayWidth, string> = {
  comfortable: '48rem',
  wide: '64rem',
  ultra: '80rem',
}

const FONT_SIZE_VALUES: Record<DisplayFontSize, string> = {
  compact: '0.875rem',
  standard: '0.9375rem',
  large: '1.0625rem',
}

const LINE_HEIGHT_VALUES: Record<DisplayLineHeight, string> = {
  tight: '1.55',
  standard: '1.72',
  loose: '1.95',
}

const MESSAGE_GAP_VALUES: Record<DisplayMessageGap, string> = {
  compact: '0.75rem',
  standard: '1.25rem',
  loose: '2rem',
}

function pick<T extends string>(value: unknown, allowed: readonly T[], fallback: T): T {
  return typeof value === 'string' && (allowed as readonly string[]).includes(value)
    ? (value as T)
    : fallback
}

function normalize(raw: Partial<DisplaySettings> | null | undefined): DisplaySettings {
  return {
    density: pick(raw?.density, ['compact', 'standard', 'comfortable'], 'standard'),
    sound: typeof raw?.sound === 'boolean' ? raw.sound : false,
    fontSize: pick(raw?.fontSize, ['compact', 'standard', 'large'], 'standard'),
    lineHeight: pick(raw?.lineHeight, ['tight', 'standard', 'loose'], 'standard'),
    messageGap: pick(raw?.messageGap, ['compact', 'standard', 'loose'], 'standard'),
    width: pick(raw?.width, ['comfortable', 'wide', 'ultra'], 'comfortable'),
  }
}

function readStored(): DisplaySettings {
  try {
    const raw = window.localStorage.getItem(STORAGE_KEY)
    if (!raw) return DEFAULT_DISPLAY_SETTINGS
    return normalize(JSON.parse(raw) as Partial<DisplaySettings>)
  } catch {
    return DEFAULT_DISPLAY_SETTINGS
  }
}

function persist(settings: DisplaySettings) {
  try {
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify(settings))
  } catch {
    /* 隐私模式：仅本次会话生效 */
  }
}

/** 把偏好映射成聊天壳的 CSS 变量（密度由 `data-density` 交给 CSS 处理）。 */
export function displayVars(settings: DisplaySettings): CSSProperties {
  return {
    '--chat-max-width': WIDTH_VALUES[settings.width],
    '--chat-font-size': FONT_SIZE_VALUES[settings.fontSize],
    '--chat-line-height': LINE_HEIGHT_VALUES[settings.lineHeight],
    '--chat-message-gap': MESSAGE_GAP_VALUES[settings.messageGap],
  } as CSSProperties
}

interface DisplaySettingsValue {
  settings: DisplaySettings
  update: (patch: Partial<DisplaySettings>) => void
  reset: () => void
}

const DisplaySettingsContext = createContext<DisplaySettingsValue | null>(null)

export function DisplaySettingsProvider({ children }: { children: ReactNode }) {
  const [settings, setSettings] = useState<DisplaySettings>(DEFAULT_DISPLAY_SETTINGS)

  useEffect(() => {
    setSettings(readStored())
    const onStorage = (event: StorageEvent) => {
      if (event.key === STORAGE_KEY) setSettings(readStored())
    }
    window.addEventListener('storage', onStorage)
    return () => window.removeEventListener('storage', onStorage)
  }, [])

  const update = useCallback((patch: Partial<DisplaySettings>) => {
    setSettings((previous) => {
      const next = normalize({ ...previous, ...patch })
      persist(next)
      return next
    })
  }, [])

  const reset = useCallback(() => {
    setSettings(DEFAULT_DISPLAY_SETTINGS)
    persist(DEFAULT_DISPLAY_SETTINGS)
  }, [])

  const value = useMemo<DisplaySettingsValue>(
    () => ({ settings, update, reset }),
    [settings, update, reset],
  )

  return (
    <DisplaySettingsContext.Provider value={value}>{children}</DisplaySettingsContext.Provider>
  )
}

export function useDisplaySettings(): DisplaySettingsValue {
  const value = useContext(DisplaySettingsContext)
  if (!value) throw new Error('useDisplaySettings must be used inside DisplaySettingsProvider')
  return value
}
