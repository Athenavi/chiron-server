#!/usr/bin/env node
/**
 * i18n 守卫：
 * 1. en 字典里不允许出现中文字符；
 * 2. app/components/lib 的字符串字面量与 JSX 文本不允许硬编码中文。
 *
 * 注释不参与检查（用 TypeScript AST 遍历，而不是 grep）。
 */

import fs from 'node:fs'
import path from 'node:path'
import ts from 'typescript'

const root = path.resolve(process.cwd())
const roots = ['app', 'components', 'lib']
const cjk = /[\u3400-\u9fff]/
const skipDirs = new Set(['node_modules', '.next', 'coverage'])
// 双语兜底与非组件 mock 数据：读取 uiText / 演示数据，不属于界面硬编码。
const skipFiles = new Set([
  'lib/mock.ts',
  'lib/ui-locale.ts',
  'lib/api.ts',
  'lib/types.ts',
])
// 开发期错误与品牌元数据，不进入界面文案。
const allow = [
  /useAuth 必须在/,
  /useI18n 必须/,
  /useTheme 必须/,
  /面向团队的多租户/,
  /^中$/,
  /^中文$/,
  // 双语兜底：这两个字面量只作为 uiText(zh, en) 的第一个参数。
  /^事件流连接失败$/,
  /^事件流中断$/,
]

const problems = []

function walk(dir) {
  if (!fs.existsSync(dir)) return
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    if (entry.name.startsWith('.') && entry.name !== '.') continue
    const full = path.join(dir, entry.name)
    if (entry.isDirectory()) {
      if (!skipDirs.has(entry.name)) walk(full)
      continue
    }
    if (!/\.(ts|tsx)$/.test(entry.name)) continue
    const relative = path.relative(root, full).replaceAll('\\', '/')
    if (skipFiles.has(relative)) continue
    if (relative.startsWith('lib/i18n/')) continue
    checkFile(full, relative)
  }
}

function checkFile(file, relative) {
  const text = fs.readFileSync(file, 'utf8')
  const source = ts.createSourceFile(
    file,
    text,
    ts.ScriptTarget.Latest,
    true,
    file.endsWith('.tsx') ? ts.ScriptKind.TSX : ts.ScriptKind.TS,
  )

  const visit = (node) => {
    const isText =
      ts.isStringLiteral(node) ||
      ts.isNoSubstitutionTemplateLiteral(node) ||
      ts.isJsxText(node) ||
      ts.isTemplateHead(node) ||
      ts.isTemplateMiddle(node) ||
      ts.isTemplateTail(node)
    if (isText && cjk.test(node.text) && !allow.some((pattern) => pattern.test(node.text))) {
      const { line } = source.getLineAndCharacterOfPosition(node.getStart(source))
      problems.push(`${relative}:${line + 1}  ${JSON.stringify(node.text.slice(0, 80))}`)
    }
    ts.forEachChild(node, visit)
  }
  visit(source)
}

function checkEnDictionaries() {
  const dir = path.join(root, 'lib', 'i18n', 'messages')
  if (!fs.existsSync(dir)) return
  for (const name of fs.readdirSync(dir)) {
    if (!name.endsWith('.en.ts')) continue
    const full = path.join(dir, name)
    const text = fs.readFileSync(full, 'utf8')
    if (!cjk.test(text)) continue
    const line = text.split('\n').findIndex((row) => cjk.test(row)) + 1
    problems.push(`lib/i18n/messages/${name}:${line}  en dictionary contains CJK`)
  }
}

for (const dir of roots) walk(path.join(root, dir))
checkEnDictionaries()

if (problems.length > 0) {
  console.error(`i18n check failed (${problems.length}):`)
  for (const problem of problems) console.error(`  ${problem}`)
  process.exit(1)
}
console.log('i18n check passed')
