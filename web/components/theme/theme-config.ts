/**
 * 主题配置：服务端（根布局内联脚本）与客户端（ThemeProvider / ThemeSwitcher）共用。
 *
 * 不依赖 React、不读写浏览器 API（除导出的脚本字符串），可被 Server Component 导入。
 *
 * 新增主题只需要两步：
 * 1. 在 `app/globals.css` 里加 `[data-theme="名称"] { ...变量覆盖... }`；
 * 2. 在本文件的 `THEME_OPTIONS` 里加一项（同时扩展 `ThemeName` 联合类型）。
 */

/** localStorage 键名（内联脚本与 Provider 必须一致）。 */
export const THEME_STORAGE_KEY = 'chiron.theme'

/** 可渲染的主题，必须与 globals.css 的 `[data-theme="..."]` 一一对应。 */
export type ThemeName = 'light' | 'dark' | 'ocean'

/** 用户偏好：具体主题，或 `system`（跟随操作系统）。 */
export type ThemePreference = 'system' | ThemeName

export type ColorScheme = 'light' | 'dark'

export interface ThemeOption {
  id: ThemePreference
  /** 该主题实际使用的 `color-scheme`。 */
  scheme: ColorScheme
  /** 切换器里的色块（任意 CSS 背景值）。 */
  swatch: string
}

/**
 * 切换器展示顺序；`system` 固定在最前面。
 *
 * 名称与说明统一走 i18n（`theme.system` / `theme.systemDesc` …），
 * 这里只保留与主题本身相关的技术信息，避免在非 React 模块里写死界面文案。
 */
export const THEME_OPTIONS: readonly ThemeOption[] = [
  {
    id: 'system',
    scheme: 'light',
    swatch: 'linear-gradient(135deg, #ffffff 0 49%, #101217 51% 100%)',
  },
  {
    id: 'light',
    scheme: 'light',
    swatch: '#f7f7f8',
  },
  {
    id: 'dark',
    scheme: 'dark',
    swatch: '#101217',
  },
  {
    id: 'ocean',
    scheme: 'light',
    swatch: '#0e7490',
  },
]

const THEME_NAMES: readonly ThemeName[] = ['light', 'dark', 'ocean']

export function isThemeName(value: unknown): value is ThemeName {
  return typeof value === 'string' && (THEME_NAMES as readonly string[]).includes(value)
}

export function isThemePreference(value: unknown): value is ThemePreference {
  return value === 'system' || isThemeName(value)
}

/** 主题 → `color-scheme`（自定义浅色主题映射为 light）。 */
export function themeScheme(theme: ThemeName): ColorScheme {
  return theme === 'dark' ? 'dark' : 'light'
}

/**
 * 首屏内联脚本：在 `<body>` 打开后立即执行，读 localStorage 并写入
 * `<html data-theme>` 与 `color-scheme`，避免 SSR 内容闪一下浅色。
 *
 * - 整体 try/catch：隐私模式下 localStorage 可能抛异常，此时保留 CSS 默认主题；
 * - 只做同步的属性写入，不阻塞首屏渲染。
 */
export const THEME_INIT_SCRIPT = `(function () {
  try {
    var key = '${THEME_STORAGE_KEY}';
    var stored = window.localStorage.getItem(key);
    var valid = stored === 'light' || stored === 'dark' || stored === 'ocean';
    var theme = valid
      ? stored
      : window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches
        ? 'dark'
        : 'light';
    var root = document.documentElement;
    root.setAttribute('data-theme', theme);
    root.style.colorScheme = theme === 'dark' ? 'dark' : 'light';
  } catch (error) {
    /* localStorage unavailable: keep the globals.css default */
  }
})();`
