'use client'

/**
 * 轻量 Markdown 渲染（不引入第三方库，也不用 `dangerouslySetInnerHTML`）。
 *
 * 支持范围：标题 1-3、段落、粗体/斜体/删除线、行内代码、围栏代码块（语言标签 +
 * 复制）、单层嵌套列表（含 `[ ]` / `[x]` 任务项）、引用、水平线、GFM 表格、
 * http(s) 链接（新窗口 + `rel="noopener noreferrer"`）。
 *
 * 设计取舍：
 * - 先按行解析成 block，再对行内文本做 token 化递归，渲染结果全是 React 元素，
 *   HTML 天然被转义成纯文本；
 * - 流式输出中未闭合的围栏代码块按「到结尾都是代码」处理，不会闪烁；
 * - 代码块复制优先 `navigator.clipboard`，不可用时静默失败（不发提示、不抛错）。
 */

import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { Button } from '@heroui/react'
import { useI18n } from '@/lib/i18n'
import { CheckIcon, CopyIcon } from '@/components/ui-bridge/icons'

// ── 类型 ────────────────────────────────────────────────────────────────

interface ListItem {
  text: string
  children: ListItem[]
}

type Block =
  | { type: 'heading'; level: 1 | 2 | 3; text: string }
  | { type: 'paragraph'; text: string }
  | { type: 'code'; lang: string; code: string }
  | { type: 'list'; ordered: boolean; items: ListItem[] }
  | { type: 'quote'; text: string }
  | { type: 'hr' }
  | { type: 'table'; header: string[]; rows: string[][] }

const FENCE_RE = /^\s*(```|~~~)\s*([^\s`]*)/
const HEADING_RE = /^(#{1,3})\s+(.*)$/
const HR_RE = /^\s*([-*_])(\s*\1){2,}\s*$/
const LIST_ITEM_RE = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/
const QUOTE_RE = /^\s*>\s?/
const TABLE_SEPARATOR_RE = /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/

/**
 * 行内 token；顺序即优先级。
 * 下划线的斜体/粗体要求两侧不是单词字符，避免把 `run_id` 之类的标识符吃掉。
 */
const INLINE_SOURCE = [
  '`[^`\\n]+`',
  '\\*\\*[\\s\\S]+?\\*\\*',
  '(?<![A-Za-z0-9])__[\\s\\S]+?__(?![A-Za-z0-9])',
  '~~[\\s\\S]+?~~',
  '\\*[^*\\n]+?\\*',
  '(?<![A-Za-z0-9])_[^_\\n]+?_(?![A-Za-z0-9])',
  '\\[[^\\]\\n]*\\]\\([^)\\s]+\\)',
  'https?:\\/\\/[^\\s<>()]+',
].join('|')

// ── 行内解析 ────────────────────────────────────────────────────────────

function sanitizeHref(raw: string): string | null {
  try {
    const url = new URL(raw)
    return url.protocol === 'http:' || url.protocol === 'https:' ? url.toString() : null
  } catch {
    return null
  }
}

function renderInline(text: string, keyPrefix: string): ReactNode[] {
  const nodes: ReactNode[] = []
  const pattern = new RegExp(INLINE_SOURCE, 'g')
  let cursor = 0
  let index = 0
  let match: RegExpExecArray | null

  while ((match = pattern.exec(text)) !== null) {
    const token = match[0]
    if (!token) {
      pattern.lastIndex += 1
      continue
    }
    if (match.index > cursor) nodes.push(text.slice(cursor, match.index))
    const key = `${keyPrefix}-i${index}`
    index += 1

    if (token.startsWith('`')) {
      nodes.push(<code key={key}>{token.slice(1, -1)}</code>)
    } else if (token.startsWith('**') || token.startsWith('__')) {
      nodes.push(<strong key={key}>{renderInline(token.slice(2, -2), key)}</strong>)
    } else if (token.startsWith('~~')) {
      nodes.push(<del key={key}>{renderInline(token.slice(2, -2), key)}</del>)
    } else if (token.startsWith('*') || token.startsWith('_')) {
      nodes.push(<em key={key}>{renderInline(token.slice(1, -1), key)}</em>)
    } else if (token.startsWith('[')) {
      const link = /^\[([^\]]*)\]\(([^)\s]+)\)$/.exec(token)
      const label = link ? link[1] : token
      const href = link ? sanitizeHref(link[2]) : null
      nodes.push(
        href ? (
          <a key={key} href={href} target="_blank" rel="noopener noreferrer">
            {renderInline(label, key)}
          </a>
        ) : (
          <span key={key}>{renderInline(label, key)}</span>
        ),
      )
    } else if (token.startsWith('http://') || token.startsWith('https://')) {
      const href = sanitizeHref(token.replace(/[.,;:!?]+$/, ''))
      nodes.push(
        href ? (
          <a key={key} href={href} target="_blank" rel="noopener noreferrer">
            {token}
          </a>
        ) : (
          token
        ),
      )
    } else {
      nodes.push(token)
    }
    cursor = match.index + token.length
  }

  if (cursor < text.length) nodes.push(text.slice(cursor))
  return nodes
}

// ── 块级解析 ────────────────────────────────────────────────────────────

function splitRow(line: string): string[] {
  const trimmed = line.trim().replace(/^\|/, '').replace(/\|$/, '')
  return trimmed.split('|').map((cell) => cell.trim())
}

function isTableStart(lines: string[], index: number): boolean {
  const header = lines[index]
  const separator = lines[index + 1]
  if (!header || separator === undefined) return false
  if (!header.includes('|')) return false
  return TABLE_SEPARATOR_RE.test(separator)
}

function isBlockStart(line: string): boolean {
  return (
    FENCE_RE.test(line) ||
    HEADING_RE.test(line) ||
    HR_RE.test(line) ||
    QUOTE_RE.test(line) ||
    LIST_ITEM_RE.test(line)
  )
}

function parseItems(lines: string[], start: number): { items: ListItem[]; next: number } {
  const items: ListItem[] = []
  let index = start
  while (index < lines.length) {
    const match = LIST_ITEM_RE.exec(lines[index])
    if (!match) break
    const indent = match[1].length
    const text = match[3]
    if (indent === 0 || items.length === 0) {
      items.push({ text, children: [] })
    } else {
      items[items.length - 1].children.push({ text, children: [] })
    }
    index += 1
  }
  return { items, next: index }
}

function parseBlocks(source: string): Block[] {
  const lines = source.replace(/\r\n?/g, '\n').split('\n')
  const blocks: Block[] = []
  let index = 0

  while (index < lines.length) {
    const line = lines[index]
    if (!line.trim()) {
      index += 1
      continue
    }

    const fence = FENCE_RE.exec(line)
    if (fence) {
      const marker = fence[1]
      const lang = fence[2] ?? ''
      const body: string[] = []
      index += 1
      while (index < lines.length && !lines[index].trimStart().startsWith(marker)) {
        body.push(lines[index])
        index += 1
      }
      if (index < lines.length) index += 1 // 吃掉收尾围栏；未闭合则一直吃到结尾
      blocks.push({ type: 'code', lang, code: body.join('\n') })
      continue
    }

    const heading = HEADING_RE.exec(line)
    if (heading) {
      const level = Math.min(3, heading[1].length) as 1 | 2 | 3
      blocks.push({ type: 'heading', level, text: heading[2].trim() })
      index += 1
      continue
    }

    if (HR_RE.test(line)) {
      blocks.push({ type: 'hr' })
      index += 1
      continue
    }

    if (isTableStart(lines, index)) {
      const header = splitRow(lines[index])
      const rows: string[][] = []
      index += 2
      while (index < lines.length && lines[index].trim() && lines[index].includes('|')) {
        rows.push(splitRow(lines[index]))
        index += 1
      }
      blocks.push({ type: 'table', header, rows })
      continue
    }

    if (QUOTE_RE.test(line)) {
      const body: string[] = []
      while (index < lines.length && QUOTE_RE.test(lines[index])) {
        body.push(lines[index].replace(QUOTE_RE, ''))
        index += 1
      }
      blocks.push({ type: 'quote', text: body.join('\n') })
      continue
    }

    const listMatch = LIST_ITEM_RE.exec(line)
    if (listMatch) {
      const ordered = /\d/.test(listMatch[2])
      const { items, next } = parseItems(lines, index)
      blocks.push({ type: 'list', ordered, items })
      index = next
      continue
    }

    const paragraph: string[] = []
    while (index < lines.length && lines[index].trim() && !isBlockStart(lines[index])) {
      paragraph.push(lines[index].trim())
      index += 1
    }
    if (paragraph.length === 0) {
      paragraph.push(line.trim())
      index += 1
    }
    blocks.push({ type: 'paragraph', text: paragraph.join(' ') })
  }

  return blocks
}

// ── 代码块 ──────────────────────────────────────────────────────────────

async function copyText(text: string): Promise<boolean> {
  try {
    if (typeof navigator !== 'undefined' && navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text)
      return true
    }
  } catch {
    // 无权限 / 非安全上下文：静默失败
  }
  return false
}

const CodeBlock = memo(function CodeBlock({ lang, code }: { lang: string; code: string }) {
  const { t } = useI18n()
  const [copied, setCopied] = useState(false)
  const timerRef = useRef<number | null>(null)

  // 卸载时清掉「已复制」回退定时器，避免流式重渲染后 setState 到已卸载组件
  useEffect(() => {
    return () => {
      if (timerRef.current !== null) window.clearTimeout(timerRef.current)
    }
  }, [])

  async function handleCopy() {
    const ok = await copyText(code)
    if (!ok) return
    setCopied(true)
    if (timerRef.current !== null) window.clearTimeout(timerRef.current)
    timerRef.current = window.setTimeout(() => {
      timerRef.current = null
      setCopied(false)
    }, 1600)
  }

  return (
    <div className="my-3 overflow-hidden rounded-lg border border-border bg-[var(--app-code-bg)]">
      <div className="flex items-center justify-between gap-2 border-b border-white/10 px-3 py-1">
        <span className="font-mono text-[11px] tracking-wide text-white/65">
          {lang || 'text'}
        </span>
        <Button
          variant="ghost"
          size="sm"
          isIconOnly
          aria-label={copied ? t('chat.code.copied') : t('chat.code.copy')}
          className="h-10 min-h-10 w-10 min-w-10 text-white/65 hover:bg-white/10 hover:text-white sm:h-8 sm:min-h-8 sm:w-8 sm:min-w-8"
          onPress={() => void handleCopy()}
        >
          {copied ? <CheckIcon size={13} aria-hidden="true" /> : <CopyIcon size={13} aria-hidden="true" />}
        </Button>
      </div>
      <pre
        style={{
          margin: 0,
          border: 0,
          borderRadius: 0,
          background: 'transparent',
          padding: '0.75rem 0.9rem',
          overflowX: 'auto',
          fontSize: '0.8125rem',
          lineHeight: 1.65,
          color: 'color-mix(in oklab, #ffffff 88%, var(--accent))',
        }}
      >
        <code>{code}</code>
      </pre>
    </div>
  )
})

// ── 列表 / 表格 ─────────────────────────────────────────────────────────

function renderListItem(item: ListItem, key: string): ReactNode {
  const task = /^\[( |x|X)\]\s+([\s\S]*)$/.exec(item.text)
  return (
    <li key={key}>
      {task ? (
        <>
          <span
            aria-hidden="true"
            className={
              'mr-1.5 inline-flex h-3.5 w-3.5 translate-y-[2px] items-center justify-center rounded-[3px] border ' +
              (task[1] === ' '
                ? 'border-border text-transparent'
                : 'border-accent bg-accent text-accent-foreground')
            }
          >
            <CheckIcon size={10} />
          </span>
          {renderInline(task[2], key)}
        </>
      ) : (
        renderInline(item.text, key)
      )}
      {item.children.length > 0 ? (
        <ul className="mt-1">{item.children.map((child, idx) => renderListItem(child, `${key}-${idx}`))}</ul>
      ) : null}
    </li>
  )
}

function renderBlock(block: Block, key: string): ReactNode {
  switch (block.type) {
    case 'heading': {
      const content = renderInline(block.text, key)
      if (block.level === 1) return <h1 key={key}>{content}</h1>
      if (block.level === 2) return <h2 key={key}>{content}</h2>
      return <h3 key={key}>{content}</h3>
    }
    case 'paragraph':
      return <p key={key}>{renderInline(block.text, key)}</p>
    case 'code':
      return <CodeBlock key={key} lang={block.lang} code={block.code} />
    case 'hr':
      return <hr key={key} />
    case 'quote':
      return <blockquote key={key}>{renderInline(block.text.replace(/\n/g, ' '), key)}</blockquote>
    case 'list': {
      const items = block.items.map((item, idx) => renderListItem(item, `${key}-${idx}`))
      return block.ordered ? <ol key={key}>{items}</ol> : <ul key={key}>{items}</ul>
    }
    case 'table':
      return (
        <div key={key} className="my-3 overflow-x-auto">
          <table>
            <thead>
              <tr>
                {block.header.map((cell, idx) => (
                  <th key={`${key}-h${idx}`}>{renderInline(cell, `${key}-h${idx}`)}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {block.rows.map((row, rowIdx) => (
                <tr key={`${key}-r${rowIdx}`}>
                  {block.header.map((_, cellIdx) => (
                    <td key={`${key}-r${rowIdx}c${cellIdx}`}>
                      {renderInline(row[cellIdx] ?? '', `${key}-r${rowIdx}c${cellIdx}`)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )
  }
}

// ── 导出 ────────────────────────────────────────────────────────────────

export interface MarkdownProps {
  text: string
  className?: string
}

/**
 * 纯函数式渲染：`text` 不变就不会重新解析（外面再套一层 memo 挡住无关渲染）。
 */
export const Markdown = memo(function Markdown({ text, className }: MarkdownProps) {
  const blocks = useMemo(() => parseBlocks(text), [text])
  return (
    <div className={className ? `prose-chat ${className}` : 'prose-chat'}>
      {blocks.map((block, index) => renderBlock(block, `b${index}`))}
    </div>
  )
})
