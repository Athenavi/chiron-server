'use client'

import Link from 'next/link'
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from 'react'
import {
  DEFAULT_LOCALE,
  LOCALE_STORAGE_KEY,
  messages,
  type Locale,
  type MessageKey,
  type MessageParams,
} from './messages'

export type { Locale, MessageKey, MessageParams }
export { DEFAULT_LOCALE, LOCALE_BOOTSTRAP_SCRIPT, LOCALE_STORAGE_KEY } from './messages'

type I18nValue = {
  locale: Locale
  setLocale: (locale: Locale) => void
  t: (key: MessageKey, params?: MessageParams) => string
}

const I18nContext = createContext<I18nValue | null>(null)

function isLocale(value: unknown): value is Locale {
  return value === 'zh' || value === 'en'
}

function readStoredLocale(): Locale {
  if (typeof window === 'undefined') return DEFAULT_LOCALE
  try {
    const stored = window.localStorage.getItem(LOCALE_STORAGE_KEY)
    return isLocale(stored) ? stored : DEFAULT_LOCALE
  } catch {
    return DEFAULT_LOCALE
  }
}

function applyDocumentLang(locale: Locale) {
  if (typeof document === 'undefined') return
  document.documentElement.lang = locale === 'zh' ? 'zh-CN' : 'en'
}

function format(template: string, params?: MessageParams): string {
  if (!params) return template
  return template.replace(/\{(\w+)\}/g, (match, name: string) =>
    name in params ? String(params[name]) : match,
  )
}

export function LocaleProvider({ children }: { children: React.ReactNode }) {
  // Starts from the default so server-rendered markup matches the first client
  // render; the stored preference is applied right after mount.
  const [locale, setLocaleState] = useState<Locale>(DEFAULT_LOCALE)

  useEffect(() => {
    const stored = readStoredLocale()
    setLocaleState(stored)
    applyDocumentLang(stored)
  }, [])

  const setLocale = useCallback((next: Locale) => {
    setLocaleState(next)
    if (typeof window !== 'undefined') {
      try {
        window.localStorage.setItem(LOCALE_STORAGE_KEY, next)
      } catch {
        // storage unavailable (private mode); keep the in-memory value
      }
    }
    applyDocumentLang(next)
  }, [])

  // `t` keeps a stable identity so page effects that use it do not re-run
  // (and re-fetch) when the locale changes.
  const localeRef = useRef(locale)
  localeRef.current = locale

  const t = useCallback((key: MessageKey, params?: MessageParams) => {
    const active = localeRef.current
    const dictionary = messages[active] ?? messages[DEFAULT_LOCALE]
    const template = dictionary[key] ?? messages[DEFAULT_LOCALE][key] ?? key
    return format(template, params)
  }, [])

  const value = useMemo<I18nValue>(() => ({ locale, setLocale, t }), [locale, setLocale, t])

  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>
}

export function useI18n(): I18nValue {
  const value = useContext(I18nContext)
  if (!value) throw new Error('useI18n must be used inside <LocaleProvider>')
  return value
}

const LOCALE_OPTIONS: { code: Locale; label: string }[] = [
  { code: 'zh', label: '中文' },
  { code: 'en', label: 'English' },
]

export function LocaleSwitcher() {
  const { locale, setLocale, t } = useI18n()
  return (
    <div
      className="row"
      role="group"
      aria-label={t('nav.localeSwitcherLabel')}
      style={{ gap: 6, marginLeft: 'auto' }}
    >
      {LOCALE_OPTIONS.map(option => (
        <button
          key={option.code}
          type="button"
          onClick={() => setLocale(option.code)}
          aria-pressed={locale === option.code}
          className={locale === option.code ? 'primary' : undefined}
          style={{ padding: '4px 10px', fontSize: 12 }}
        >
          {option.label}
        </button>
      ))}
    </div>
  )
}

export function ConsoleNav() {
  const { t } = useI18n()
  return (
    <header>
      <strong>Chiron Platform</strong>
      <Link href="/tenants">{t('nav.tenants')}</Link>
      <Link href="/plans">{t('nav.plans')}</Link>
      <Link href="/usage">{t('nav.usage')}</Link>
      <Link href="/migrations">{t('nav.migrations')}</Link>
      <Link href="/integrations">{t('nav.integrations')}</Link>
      <Link href="/settings">{t('nav.settings')}</Link>
      <LocaleSwitcher />
    </header>
  )
}
