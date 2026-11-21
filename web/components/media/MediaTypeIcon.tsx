/**
 * 非图片文件的类型图标：按扩展名（再回落到 MIME）选择图标与配色，
 * 扩展名本身由卡片以等宽徽标展示。
 */

import type { ReactElement } from 'react'
import {
  ArchiveIcon,
  CodeIcon,
  DocIcon,
  FileIcon,
  PlayIcon,
  SheetIcon,
  type MediaIconProps,
} from './MediaIcons'
import { extensionOf } from './media-utils'

interface IconChoice {
  Icon: (props: MediaIconProps) => ReactElement
  /** 图标颜色类（HeroUI 语义色）。 */
  tone: string
}

const CODE_EXT = new Set([
  'js',
  'jsx',
  'ts',
  'tsx',
  'json',
  'html',
  'htm',
  'css',
  'scss',
  'py',
  'go',
  'rs',
  'java',
  'c',
  'h',
  'cpp',
  'sh',
  'yml',
  'yaml',
  'toml',
  'xml',
  'sql',
])

const DOC_EXT = new Set(['txt', 'md', 'markdown', 'pdf', 'doc', 'docx', 'rtf', 'log', 'csv'])
const SHEET_EXT = new Set(['xls', 'xlsx', 'numbers', 'ods'])
const ARCHIVE_EXT = new Set(['zip', 'rar', '7z', 'gz', 'tgz', 'tar', 'bz2'])
const MEDIA_EXT = new Set(['mp4', 'mov', 'webm', 'mkv', 'mp3', 'wav', 'ogg', 'flac', 'm4a'])

function choiceFor(ext: string, mime: string): IconChoice {
  if (ARCHIVE_EXT.has(ext)) return { Icon: ArchiveIcon, tone: 'text-warning' }
  if (SHEET_EXT.has(ext)) return { Icon: SheetIcon, tone: 'text-success' }
  if (DOC_EXT.has(ext)) return { Icon: DocIcon, tone: 'text-accent' }
  if (CODE_EXT.has(ext)) return { Icon: CodeIcon, tone: 'text-accent' }
  if (MEDIA_EXT.has(ext)) return { Icon: PlayIcon, tone: 'text-danger' }

  // 没有可用扩展名时按 MIME 大类兜底
  if (mime.startsWith('text/')) return { Icon: DocIcon, tone: 'text-accent' }
  if (mime.startsWith('audio/') || mime.startsWith('video/')) {
    return { Icon: PlayIcon, tone: 'text-danger' }
  }
  if (mime.includes('zip') || mime.includes('compressed') || mime.includes('tar')) {
    return { Icon: ArchiveIcon, tone: 'text-warning' }
  }
  return { Icon: FileIcon, tone: 'text-muted' }
}

export interface MediaTypeIconProps {
  filename: string
  mime: string
  size?: number
  className?: string
}

export function MediaTypeIcon({ filename, mime, size = 32, className }: MediaTypeIconProps) {
  const { Icon, tone } = choiceFor(extensionOf(filename), (mime || '').toLowerCase())
  return <Icon size={size} className={[tone, className].filter(Boolean).join(' ')} />
}
