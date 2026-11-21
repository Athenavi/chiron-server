'use client'

/**
 * 移动端可视视口高度。
 *
 * iOS 键盘弹出时 `100dvh` 不一定收缩，聊天页改用 `visualViewport.height` 作为高度，
 * 让输入框始终留在键盘上方；不支持 `visualViewport` 的浏览器返回 null，沿用 dvh。
 */

import { useEffect, useState } from 'react'

export interface VisualViewportState {
  height: number | null
  /** 键盘可能已弹出：布局视口与可视视口的高度差超过阈值。 */
  keyboardOpen: boolean
}

const KEYBOARD_THRESHOLD = 150

export function useVisualViewport(): VisualViewportState {
  const [state, setState] = useState<VisualViewportState>({
    height: null,
    keyboardOpen: false,
  })

  useEffect(() => {
    const viewport = window.visualViewport
    if (!viewport) return

    const update = () => {
      const height = Math.round(viewport.height)
      const overlap = Math.round(window.innerHeight - viewport.height)
      setState({ height, keyboardOpen: overlap > KEYBOARD_THRESHOLD })
    }

    update()
    viewport.addEventListener('resize', update)
    viewport.addEventListener('scroll', update)
    return () => {
      viewport.removeEventListener('resize', update)
      viewport.removeEventListener('scroll', update)
    }
  }, [])

  return state
}
