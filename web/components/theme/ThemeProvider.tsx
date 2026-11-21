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
import {
  THEME_OPTIONS,
  THEME_STORAGE_KEY,
  isThemePreference,
  themeScheme,
  type ColorScheme,
  type ThemeName,
  type ThemeOption,
  type ThemePreference,
} from './theme-config'

interface ThemeContextValue {
  /** 用户偏好（可能为 `system`）。 */
  theme: ThemePreference
  /** 实际生效的主题（`system` 已解析为 `light` / `dark`）。 */
  resolved: ThemeName
  setTheme: (theme: ThemePreference) => void
  /** 可选主题列表（含"跟随系统"）。 */
  themes: readonly ThemeOption[]
}

const ThemeContext = createContext<ThemeContextValue | null>(null)

export function useTheme(): ThemeContextValue {
  const context = useContext(ThemeContext)
  if (!context) throw new Error('useTheme 必须在 ThemeProvider 内使用')
  return context
}

const DARK_QUERY = '(prefers-color-scheme: dark)'

function readSystemScheme(): ColorScheme {
  if (typeof window === 'undefined' || typeof window.matchMedia !== 'function') return 'light'
  return window.matchMedia(DARK_QUERY).matches ? 'dark' : 'light'
}

function applyTheme(theme: ThemeName): void {
  if (typeof document === 'undefined') return
  const root = document.documentElement
  root.setAttribute('data-theme', theme)
  // 让浏览器原生控件（滚动条、输入框、下拉）跟随主题
  root.style.colorScheme = themeScheme(theme)
}

/**
 * 主题上下文。
 *
 * 首屏由 `app/layout.tsx` 里的内联脚本写好 `<html data-theme>`，这里只负责之后的状态：
 * - 挂载后读取 localStorage 恢复偏好；
 * - 偏好为 `system` 时监听 `prefers-color-scheme` 变化；
 * - 每次变化写回 `<html data-theme>` + `color-scheme`；
 * - 监听 `storage` 事件，多标签页保持一致。
 */
export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<ThemePreference>('system')
  const [systemScheme, setSystemScheme] = useState<ColorScheme>('light')
  // 首屏脚本已经把主题写进 DOM，等这里读完偏好再同步，避免闪一下默认主题
  const [hydrated, setHydrated] = useState(false)

  useEffect(() => {
    let stored: string | null = null
    try {
      stored = window.localStorage.getItem(THEME_STORAGE_KEY)
    } catch {
      stored = null
    }
    setThemeState(isThemePreference(stored) ? stored : 'system')
    setSystemScheme(readSystemScheme())
    setHydrated(true)
  }, [])

  useEffect(() => {
    if (typeof window.matchMedia !== 'function') return
    const query = window.matchMedia(DARK_QUERY)
    const onChange = () => setSystemScheme(query.matches ? 'dark' : 'light')
    query.addEventListener('change', onChange)
    return () => query.removeEventListener('change', onChange)
  }, [])

  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key !== THEME_STORAGE_KEY) return
      setThemeState(isThemePreference(event.newValue) ? event.newValue : 'system')
    }
    window.addEventListener('storage', onStorage)
    return () => window.removeEventListener('storage', onStorage)
  }, [])

  const resolved: ThemeName = theme === 'system' ? systemScheme : theme

  useEffect(() => {
    if (!hydrated) return
    applyTheme(resolved)
  }, [hydrated, resolved])

  const setTheme = useCallback((next: ThemePreference) => {
    setThemeState(next)
    try {
      window.localStorage.setItem(THEME_STORAGE_KEY, next)
    } catch {
      // 隐私模式下无法持久化：本次会话内仍然生效
    }
    // 立即应用，不等下一次 effect，避免自定义主题切换有可感知延迟
    applyTheme(next === 'system' ? readSystemScheme() : next)
  }, [])

  const value = useMemo<ThemeContextValue>(
    () => ({ theme, resolved, setTheme, themes: THEME_OPTIONS }),
    [theme, resolved, setTheme],
  )

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>
}
