/** 轻量工具函数（不引入 clsx/tailwind-merge 等依赖）。 */

export function cn(...values: Array<string | false | null | undefined>): string {
  return values.filter(Boolean).join(' ')
}

/**
 * `2026-11-21T10:00:00Z` → 本地 `11-21 18:00`。
 *
 * 第二个参数是当前界面语言（`useI18n().locale`）；不传时保持中文口径，
 * 供少数非 React 场景调用。日期只做展示，切换语言后由调用方重新渲染即可。
 */
export function formatDateTime(
  value: string | null | undefined,
  locale: 'zh' | 'en' = 'zh',
): string {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  return date.toLocaleString(locale === 'en' ? 'en-US' : 'zh-CN', {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  })
}

/** 头像缩写：中文取末两字，英文取首字母。 */
export function initials(name: string | null | undefined): string {
  const trimmed = (name ?? '').trim()
  if (!trimmed) return '?'
  if (/^[\u4e00-\u9fa5]+$/.test(trimmed)) return trimmed.slice(-2)
  const parts = trimmed.split(/\s+/).filter(Boolean)
  if (parts.length === 1) return parts[0].slice(0, 2).toUpperCase()
  return `${parts[0][0]}${parts[parts.length - 1][0]}`.toUpperCase()
}
