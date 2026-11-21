/**
 * 记忆页的展示元数据：槽位配色、来源/作用域映射与时间格式化。
 *
 * 文案全部走 i18n（`memory.*`），因此这里只做映射与格式化，由调用方传入翻译函数
 * 与当前 locale；不含组件状态。页面、卡片与表单共用，避免同一套文案多处维护。
 */

import type { MemorySlot } from '@/lib/types'

/** Chip 的可用色板（与 @heroui/styles 的 chipVariants.color 对齐）。 */
export type SlotColor = 'accent' | 'success' | 'warning' | 'danger'

/** `useI18n().t` 的签名，便于在非 hook 模块里复用文案。 */
export type Translator = (key: string, vars?: Record<string, string | number>) => string

export interface SlotMeta {
  id: MemorySlot
  color: SlotColor
}

/** 后端返回未知槽位时也不至于渲染成空白。 */
const UNKNOWN_SLOT: SlotMeta = { id: 'facts', color: 'accent' }

export const SLOT_META: Record<MemorySlot, SlotMeta> = {
  persona: { id: 'persona', color: 'accent' },
  preferences: { id: 'preferences', color: 'success' },
  facts: { id: 'facts', color: 'warning' },
  summary: { id: 'summary', color: 'danger' },
}

export const SLOT_ORDER: MemorySlot[] = ['persona', 'preferences', 'facts', 'summary']

export function slotMeta(slot: MemorySlot): SlotMeta {
  return SLOT_META[slot] ?? UNKNOWN_SLOT
}

/** 槽位展示名（`memory.slot.*`）。 */
export function slotLabel(slot: MemorySlot, t: Translator): string {
  return t(`memory.slot.${SLOT_META[slot] ? slot : 'unknown'}`)
}

/** 槽位补充说明（表单 / 筛选下拉里展示）。 */
export function slotDescription(slot: MemorySlot, t: Translator): string {
  return t(`memory.slotDescription.${SLOT_META[slot] ? slot : 'unknown'}`)
}

/** 按 slot 筛选的选项（`all` 表示不筛选）。 */
export type SlotFilter = MemorySlot | 'all'

export function filterOptions(t: Translator): { id: SlotFilter; label: string }[] {
  return [
    { id: 'all', label: t('memory.filter.all') },
    ...SLOT_ORDER.map((slot) => ({ id: slot, label: slotLabel(slot, t) })),
  ]
}

const SOURCES = ['user', 'auto', 'import'] as const

/** 来源展示名；未知来源回落为后端原值，再回落为「未知」。 */
export function sourceLabel(source: string, t: Translator): string {
  const key = (source ?? '').trim().toLowerCase()
  if ((SOURCES as readonly string[]).includes(key)) return t(`memory.source.${key}`)
  return (source ?? '').trim() || t('memory.source.unknown')
}

/** 表单里可选的来源。 */
export function sourceOptions(t: Translator): { id: string; label: string }[] {
  return SOURCES.map((id) => ({ id, label: t(`memory.sourceOption.${id}`) }))
}

export const GLOBAL_SCOPE = 'global'

export function isSessionScope(scope: string): boolean {
  return scope.startsWith('session:')
}

/** `global` → 全局；`session:<uuid>` → 会话 · 前 8 位。 */
export function scopeLabel(scope: string, t: Translator): string {
  const value = scope.trim()
  if (!value || value === GLOBAL_SCOPE || value === 'user') return t('memory.scope.global')
  if (isSessionScope(value)) {
    const id = value.slice('session:'.length)
    return t('memory.scope.session', { id: id.slice(0, 8) || t('memory.scope.unnamed') })
  }
  return value
}

/** 0.86 → 86%；非数字时回落为 —。 */
export function confidenceLabel(confidence: number): string {
  if (typeof confidence !== 'number' || !Number.isFinite(confidence)) return '—'
  return `${Math.round(confidence * 100)}%`
}

function intlLocale(locale: string): string {
  return locale === 'en' ? 'en-US' : 'zh-CN'
}

/** 绝对时间（title 提示用）：`11/21 09:30`。 */
export function formatTimestamp(value: string | null | undefined, locale: string): string {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  return date.toLocaleString(intlLocale(locale), {
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    hour12: false,
  })
}

/** 相对时间：刚刚 / 3 分钟前 / 昨天 / 11/21（按 locale 输出）。 */
export function relativeLabel(
  value: string | null | undefined,
  t: Translator,
  locale: string,
): string {
  if (!value) return '—'
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return '—'
  const diff = Date.now() - date.getTime()
  const minute = 60_000
  const hour = 60 * minute
  const day = 24 * hour
  if (diff < minute) return t('memory.relative.justNow')
  if (diff < hour) return t('memory.relative.minutes', { count: Math.floor(diff / minute) })
  if (diff < day) return t('memory.relative.hours', { count: Math.floor(diff / hour) })
  if (diff < 2 * day) return t('memory.relative.yesterday')
  return date.toLocaleDateString(intlLocale(locale), { month: '2-digit', day: '2-digit' })
}

export function updatedLabel(updatedAt: string, t: Translator, locale: string): string {
  return relativeLabel(updatedAt, t, locale)
}

export function updatedTitle(updatedAt: string, locale: string): string {
  return formatTimestamp(updatedAt, locale)
}

/** 卡片 aria-label 用的内容摘要（避免整段内容进入无障碍名称）。 */
export function contentPreview(content: string, max = 24): string {
  const flat = content.replace(/\s+/g, ' ').trim()
  return flat.length > max ? `${flat.slice(0, max)}…` : flat
}
