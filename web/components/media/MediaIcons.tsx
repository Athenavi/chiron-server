/**
 * 媒体页专用图标（内联 SVG）。
 *
 * `components/ui-bridge/icons` 不在本页面的改动范围内，这里按同样的
 * 24×24 / `currentColor` / 1.75 线宽约定补上上传、下载与文件类型图标。
 */

import type { SVGProps } from 'react'

export type MediaIconProps = SVGProps<SVGSVGElement> & { size?: number }

function MediaIcon({ size = 16, children, ...props }: MediaIconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      {...props}
    >
      {children}
    </svg>
  )
}

export function UploadIcon(props: MediaIconProps) {
  return (
    <MediaIcon {...props}>
      <path d="M12 15V4" />
      <path d="m7.5 8.5 4.5-4.5 4.5 4.5" />
      <path d="M4 15v3.5A1.5 1.5 0 0 0 5.5 20h13a1.5 1.5 0 0 0 1.5-1.5V15" />
    </MediaIcon>
  )
}

export function DownloadIcon(props: MediaIconProps) {
  return (
    <MediaIcon {...props}>
      <path d="M12 4v11" />
      <path d="m7.5 10.5 4.5 4.5 4.5-4.5" />
      <path d="M4 15v3.5A1.5 1.5 0 0 0 5.5 20h13a1.5 1.5 0 0 0 1.5-1.5V15" />
    </MediaIcon>
  )
}

export function ImageIcon(props: MediaIconProps) {
  return (
    <MediaIcon {...props}>
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <circle cx="8.5" cy="9.5" r="1.5" />
      <path d="m4 17 4.5-4.5L12 16l2.5-2.5L20 19" />
    </MediaIcon>
  )
}

export function FileIcon(props: MediaIconProps) {
  return (
    <MediaIcon {...props}>
      <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
      <path d="M14 3v5h5" />
    </MediaIcon>
  )
}

export function DocIcon(props: MediaIconProps) {
  return (
    <MediaIcon {...props}>
      <path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z" />
      <path d="M14 3v5h5" />
      <path d="M9 13h6" />
      <path d="M9 17h4" />
    </MediaIcon>
  )
}

export function CodeIcon(props: MediaIconProps) {
  return (
    <MediaIcon {...props}>
      <path d="m8.5 9-3.5 3.5L8.5 16" />
      <path d="m15.5 9 3.5 3.5-3.5 3.5" />
      <path d="m13.5 7-3 10" />
    </MediaIcon>
  )
}

export function ArchiveIcon(props: MediaIconProps) {
  return (
    <MediaIcon {...props}>
      <path d="M3 6.5A1.5 1.5 0 0 1 4.5 5h15A1.5 1.5 0 0 1 21 6.5V8H3z" />
      <path d="M4.5 8v10A2 2 0 0 0 6.5 20h11a2 2 0 0 0 2-2V8" />
      <path d="M10 12h4" />
    </MediaIcon>
  )
}

export function SheetIcon(props: MediaIconProps) {
  return (
    <MediaIcon {...props}>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2" />
      <path d="M3.5 9.5h17" />
      <path d="M9.5 9.5v10" />
      <path d="M15 9.5v10" />
    </MediaIcon>
  )
}

export function PlayIcon(props: MediaIconProps) {
  return (
    <MediaIcon {...props}>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M10.5 9.2v5.6l4.6-2.8z" />
    </MediaIcon>
  )
}
