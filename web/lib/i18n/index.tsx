'use client'

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react'
import zh from './messages/zh'
import en from './messages/en'

export type Locale = 'zh' | 'en'
export type Messages = typeof zh

const STORAGE_KEY = 'chiron.locale'
const DICTS: Record<Locale, Messages> = { zh, en }

type Vars = Record<string, string | number>

interface I18nValue {
  locale: Locale
  setLocale: (locale: Locale) => void
  /** 例如 `t('nav.chat')`；缺失键回落到中文，再回落 key 本身。 */
  t: (key: string, vars?: Vars) => string
}

const I18nContext = createContext<I18nValue | null>(null)

function lookup(dict: unknown, key: string): string | undefined {
  return key.split('.').reduce<unknown>((node, part) => {
    if (node && typeof node === 'object' && part in (node as Record<string, unknown>)) {
      return (node as Record<string, unknown>)[part]
    }
    return undefined
  }, dict) as string | undefined
}

function interpolate(template: string, vars?: Vars): string {
  if (!vars) return template
  return template.replace(/\{(\w+)\}/g, (_, name: string) =>
    name in vars ? String(vars[name]) : `{${name}}`,
  )
}

function normalizeLocale(value: string | null | undefined): Locale | null {
  if (!value) return null
  return value.toLowerCase().startsWith('en') ? 'en' : 'zh'
}

export function I18nProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<Locale>('zh')

  useEffect(() => {
    let stored: string | null = null
    try {
      stored = window.localStorage.getItem(STORAGE_KEY)
    } catch {
      stored = null
    }
    const next = normalizeLocale(stored) ?? normalizeLocale(navigator.language) ?? 'zh'
    setLocaleState(next)
    document.documentElement.lang = next === 'en' ? 'en' : 'zh-CN'
  }, [])

  const setLocale = useCallback((next: Locale) => {
    setLocaleState(next)
    document.documentElement.lang = next === 'en' ? 'en' : 'zh-CN'
    try {
      window.localStorage.setItem(STORAGE_KEY, next)
    } catch {
      /* private mode：仅本次会话生效 */
    }
  }, [])

  const t = useCallback(
    (key: string, vars?: Vars) => {
      const primary = lookup(DICTS[locale], key)
      const fallback = lookup(DICTS.zh, key)
      return interpolate(primary ?? fallback ?? key, vars)
    },
    [locale],
  )

  const value = useMemo<I18nValue>(() => ({ locale, setLocale, t }), [locale, setLocale, t])
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>
}

export function useI18n(): I18nValue {
  const ctx = useContext(I18nContext)
  if (!ctx) throw new Error('useI18n must be used inside <I18nProvider>')
  return ctx
}

/** 非 React 场景（如发送给后端的错误文案映射）可用。 */
export function translate(locale: Locale, key: string, vars?: Vars): string {
  return interpolate(lookup(DICTS[locale], key) ?? lookup(DICTS.zh, key) ?? key, vars)
}
