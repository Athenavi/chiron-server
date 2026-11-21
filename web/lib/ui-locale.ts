/**
 * React 之外的界面语言兜底。
 *
 * `lib/types.ts` / `lib/api.ts` 在 Provider 之外产生错误文案，不能调用 `useI18n`；
 * 这里读同一个 localStorage 键，保证 en 界面不会混入硬编码中文。
 */

export function currentUiLocale(): 'zh' | 'en' {
  if (typeof window === 'undefined') return 'zh'
  try {
    return window.localStorage.getItem('chiron.locale')?.toLowerCase().startsWith('en')
      ? 'en'
      : 'zh'
  } catch {
    return 'zh'
  }
}

export function uiText(zh: string, en: string): string {
  return currentUiLocale() === 'en' ? en : zh
}
