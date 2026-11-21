/**
 * 模型页的纯函数工具：
 * - 读取套餐白名单（`Plan` 类型未声明 `model_allowlist`，后端按需下发，这里宽松解析）；
 * - 按前缀判断 Provider 是否命中白名单；
 * - 本地搜索过滤（label / provider / 协议 / base_url / 前缀）。
 */

import type { ModelProvider, Plan } from '@/lib/types'

/** 套餐白名单：过滤空值与非字符串，保持原顺序并去重。 */
export function modelAllowlistOf(plan: Plan | null): string[] {
  const raw = (plan as (Plan & { model_allowlist?: unknown }) | null)?.model_allowlist
  if (!Array.isArray(raw)) return []
  const seen = new Set<string>()
  const result: string[] = []
  for (const item of raw) {
    if (typeof item !== 'string') continue
    const value = item.trim()
    if (!value || seen.has(value)) continue
    seen.add(value)
    result.push(value)
  }
  return result
}

/**
 * 前缀命中：完全相等，或一方是另一方的前缀
 * （`openai:` 命中 `openai:gpt-5.2`，`gpt-` 命中 `gpt-5.2`）。
 */
export function prefixMatches(prefix: string, entry: string): boolean {
  const left = prefix.trim().toLowerCase()
  const right = entry.trim().toLowerCase()
  if (!left || !right) return false
  return left === right || left.startsWith(right) || right.startsWith(left)
}

/**
 * Provider 是否在套餐范围内。
 *
 * 白名单条目既可能是 `provider:` 前缀，也可能是裸模型名前缀；
 * 因此把 provider id、`provider:` 与 `model_prefixes` 都作为候选前缀。
 */
export function isProviderAllowed(provider: ModelProvider, allowlist: string[]): boolean {
  if (allowlist.length === 0) return true
  const prefixes = [provider.provider, `${provider.provider}:`, ...provider.model_prefixes]
  return prefixes.some((prefix) => allowlist.some((entry) => prefixMatches(prefix, entry)))
}

/** 本地搜索：命中任一可见字段即保留。 */
export function filterProviders(providers: ModelProvider[], query: string): ModelProvider[] {
  const keyword = query.trim().toLowerCase()
  if (!keyword) return providers
  return providers.filter((provider) => {
    const fields = [
      provider.label,
      provider.provider,
      provider.protocol,
      provider.base_url ?? '',
      ...provider.model_prefixes,
    ]
    return fields.some((field) => field.toLowerCase().includes(keyword))
  })
}

/** upstream 兜底：`deepseek:deepseek-chat` → `deepseek-chat`（裸模型名原样返回）。 */
export function upstreamOf(model: string): string {
  const value = model.trim()
  const index = value.indexOf(':')
  return index > 0 ? value.slice(index + 1) : value
}

/**
 * 解析区间内的整数（表单输入用）：空串 / 非数字 / 越界都返回 null，
 * 由调用方决定是提示错误还是回落到默认值。
 */
export function parseBoundedInt(text: string, min: number, max: number): number | null {
  const raw = text.trim()
  if (!raw) return null
  // 只接受纯数字，避免 `1e3` / `1.5` 之类被 Number 悄悄接受。
  if (!/^\d+$/.test(raw)) return null
  const value = Number(raw)
  if (!Number.isFinite(value) || value < min || value > max) return null
  return value
}
