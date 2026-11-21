/** 媒体页的纯函数工具（大小格式化、扩展名解析）。 */

/** 文件大小：B / KB / MB（后端上限 50 MiB，不会到 GB）。 */
export function formatBytes(bytes: number | null | undefined): string {
  if (typeof bytes !== 'number' || !Number.isFinite(bytes) || bytes < 0) return '—'
  if (bytes < 1024) return `${Math.round(bytes)} B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb < 10 ? kb.toFixed(1) : Math.round(kb)} KB`
  const mb = kb / 1024
  return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`
}

/** 小写扩展名（不含点）；没有扩展名时返回空串。 */
export function extensionOf(filename: string): string {
  const index = filename.lastIndexOf('.')
  if (index <= 0 || index === filename.length - 1) return ''
  return filename.slice(index + 1).toLowerCase()
}
