#!/usr/bin/env node
/**
 * Console i18n gate (no dependencies).
 *
 * Fails when user-facing CJK text is hardcoded outside the dictionary:
 *   - scans console/app and console/lib (.ts/.tsx) for CJK in string literals
 *     and JSX text (comments are ignored)
 *   - allowlists the locale labels `中` / `中文` (used by the locale switcher)
 *   - allows a per-line escape hatch: a line containing `i18n-allow-cjk`
 *   - validates console/lib/messages.ts: zh/en key parity and no CJK in `en`
 *
 * Usage: node scripts/check-i18n.mjs  (or: npm run check:i18n)
 */

import { readFileSync, readdirSync, statSync } from 'node:fs'
import { join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const CONSOLE_ROOT = resolve(fileURLToPath(new URL('..', import.meta.url)))
const SCAN_DIRS = ['app', 'lib']
const SCAN_EXT = new Set(['.ts', '.tsx'])
const DICTIONARY_FILE = join(CONSOLE_ROOT, 'lib', 'messages.ts')
const ALLOWED_CJK_STRINGS = new Set(['中', '中文'])
const ESCAPE_HATCH = 'i18n-allow-cjk'

// CJK ideographs, CJK punctuation, fullwidth forms.
const CJK = /[\u3000-\u303f\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\ufe30-\ufe4f\uff00-\uffef]/
const CJK_GLOBAL = new RegExp(CJK.source, 'g')

/** @param {string} dir @returns {string[]} */
function walk(dir) {
  /** @type {string[]} */
  const files = []
  let entries
  try {
    entries = readdirSync(dir)
  } catch {
    return files
  }
  for (const entry of entries) {
    if (entry === 'node_modules' || entry === '.next' || entry.startsWith('.')) continue
    const full = join(dir, entry)
    if (statSync(full).isDirectory()) files.push(...walk(full))
    else if (SCAN_EXT.has(extname(full))) files.push(full)
  }
  return files
}

/** @param {string} file */
function extname(file) {
  const dot = file.lastIndexOf('.')
  return dot < 0 ? '' : file.slice(dot)
}

/** @param {string} source @param {number} index */
function position(source, index) {
  let line = 1
  let lineStart = 0
  for (let i = 0; i < index; i += 1) {
    if (source[i] === '\n') {
      line += 1
      lineStart = i + 1
    }
  }
  return { line, column: index - lineStart + 1 }
}

/** @param {string} source @param {number} index */
function lineTextAt(source, index) {
  const start = source.lastIndexOf('\n', index - 1) + 1
  const end = source.indexOf('\n', index)
  return source.slice(start, end < 0 ? source.length : end)
}

/**
 * Walks the source tracking comments/strings so CJK in comments can be ignored
 * while CJK in strings and JSX text is reported.
 * @param {string} source
 * @param {string} file
 */
function scanSource(source, file) {
  /** @type {{file: string, line: number, column: number, kind: string, text: string}[]} */
  const violations = []
  let i = 0

  while (i < source.length) {
    const char = source[i]
    const next = source[i + 1]

    // comments
    if (char === '/' && next === '/') {
      const end = source.indexOf('\n', i)
      i = end < 0 ? source.length : end
      continue
    }
    if (char === '/' && next === '*') {
      const end = source.indexOf('*/', i + 2)
      i = end < 0 ? source.length : end + 2
      continue
    }

    // string literals and template literals
    if (char === "'" || char === '"' || char === '`') {
      const quote = char
      const start = i
      i += 1
      let value = ''
      while (i < source.length) {
        const current = source[i]
        if (current === '\\') {
          value += source.slice(i, i + 2)
          i += 2
          continue
        }
        if (current === quote) break
        value += current
        i += 1
      }
      i += 1 // closing quote
      if (CJK.test(value) && !ALLOWED_CJK_STRINGS.has(value.trim())) {
        const { line, column } = position(source, start)
        if (!lineTextAt(source, start).includes(ESCAPE_HATCH)) {
          violations.push({ file, line, column, kind: 'string', text: value.trim().slice(0, 80) })
        }
      }
      continue
    }

    // CJK outside comments/strings: JSX text or identifiers
    if (CJK.test(char)) {
      const { line, column } = position(source, i)
      if (!lineTextAt(source, i).includes(ESCAPE_HATCH)) {
        violations.push({ file, line, column, kind: 'text', text: lineTextAt(source, i).trim().slice(0, 80) })
      }
    }
    i += 1
  }

  return violations
}

/**
 * Extracts the object literal that follows the first `{` after `marker`.
 * @param {string} source @param {string} marker
 */
function extractObjectBody(source, marker) {
  const at = source.indexOf(marker)
  if (at < 0) return null
  let i = source.indexOf('{', at)
  if (i < 0) return null
  const start = i
  let depth = 0
  while (i < source.length) {
    const char = source[i]
    if (char === '/' && source[i + 1] === '/') {
      const end = source.indexOf('\n', i)
      i = end < 0 ? source.length : end
      continue
    }
    if (char === "'" || char === '"' || char === '`') {
      const quote = char
      i += 1
      while (i < source.length) {
        if (source[i] === '\\') {
          i += 2
          continue
        }
        if (source[i] === quote) break
        i += 1
      }
      i += 1
      continue
    }
    if (char === '{') depth += 1
    else if (char === '}') {
      depth -= 1
      if (depth === 0) return source.slice(start + 1, i)
    }
    i += 1
  }
  return null
}

/** @param {string} body */
function extractKeys(body) {
  const keys = new Set()
  const pattern = /^[ \t]*(?:'([^']+)'|"([^"]+)"|([A-Za-z_$][\w$]*))[ \t]*:/gm
  for (const match of body.matchAll(pattern)) {
    keys.add(match[1] ?? match[2] ?? match[3])
  }
  return keys
}

function checkDictionary() {
  /** @type {string[]} */
  const problems = []
  let source
  try {
    source = readFileSync(DICTIONARY_FILE, 'utf8')
  } catch {
    return [`${DICTIONARY_FILE}: missing dictionary`]
  }

  const zhBody = extractObjectBody(source, 'export const zh')
  const enBody = extractObjectBody(source, 'export const en')
  if (!zhBody) problems.push('messages.ts: could not locate the `zh` dictionary')
  if (!enBody) problems.push('messages.ts: could not locate the `en` dictionary')
  if (!zhBody || !enBody) return problems

  const zhKeys = extractKeys(zhBody)
  const enKeys = extractKeys(enBody)
  for (const key of zhKeys) if (!enKeys.has(key)) problems.push(`messages.ts: en is missing key "${key}"`)
  for (const key of enKeys) if (!zhKeys.has(key)) problems.push(`messages.ts: en has unknown key "${key}"`)

  if (CJK.test(enBody)) {
    const matches = enBody.match(CJK_GLOBAL) ?? []
    problems.push(`messages.ts: en dictionary contains CJK characters: ${[...new Set(matches)].join('')}`)
  }
  if (zhKeys.size === 0) problems.push('messages.ts: no keys found in zh dictionary')

  return problems
}

function main() {
  /** @type {string[]} */
  const violations = []
  for (const dir of SCAN_DIRS) {
    for (const file of walk(join(CONSOLE_ROOT, dir))) {
      if (resolve(file) === DICTIONARY_FILE) continue
      violations.push(...scanSource(readFileSync(file, 'utf8'), relative(CONSOLE_ROOT, file).replace(/\\/g, '/')))
    }
  }

  const dictionaryProblems = checkDictionary()

  for (const problem of dictionaryProblems) console.error(`  ERROR ${problem}`)
  for (const v of violations) {
    console.error(`  ERROR ${v.file}:${v.line}:${v.column} hardcoded CJK ${v.kind} -> ${v.text}`)
  }

  if (violations.length === 0 && dictionaryProblems.length === 0) {
    console.log(`i18n check passed: no hardcoded CJK outside lib/messages.ts, zh/en keys in sync.`)
    return 0
  }

  console.error('')
  console.error(
    `i18n check failed: ${violations.length} hardcoded string(s), ${dictionaryProblems.length} dictionary issue(s).`,
  )
  console.error(`Move user-facing copy into console/lib/messages.ts and read it via t('key').`)
  return 1
}

process.exit(main())
