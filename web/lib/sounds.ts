/**
 * 聊天交互音效（Web Audio，无音频资源依赖）。
 *
 * 只在用户开启「交互音效」后调用；发送由用户手势触发，接收时 AudioContext
 * 已存在，不会被浏览器自动播放策略拦截。任何失败都静默忽略，不影响消息流。
 */

export type ChatSound = 'send' | 'receive'

let context: AudioContext | null = null

function audioContext(): AudioContext | null {
  if (typeof window === 'undefined') return null
  if (context) return context
  const Ctor =
    window.AudioContext ??
    (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
  if (!Ctor) return null
  try {
    context = new Ctor()
    return context
  } catch {
    return null
  }
}

/** 两声短音：发送上扬、接收回落，音量压得很低。 */
export function playChatSound(kind: ChatSound): void {
  const ctx = audioContext()
  if (!ctx) return
  try {
    if (ctx.state === 'suspended') void ctx.resume()
    const start = ctx.currentTime
    const notes = kind === 'send' ? [620, 880] : [880, 620]
    notes.forEach((frequency, index) => {
      const at = start + index * 0.07
      const oscillator = ctx.createOscillator()
      const gain = ctx.createGain()
      oscillator.type = 'sine'
      oscillator.frequency.setValueAtTime(frequency, at)
      gain.gain.setValueAtTime(0.0001, at)
      gain.gain.exponentialRampToValueAtTime(0.035, at + 0.012)
      gain.gain.exponentialRampToValueAtTime(0.0001, at + 0.11)
      oscillator.connect(gain)
      gain.connect(ctx.destination)
      oscillator.start(at)
      oscillator.stop(at + 0.12)
    })
  } catch {
    /* 音效不是关键路径 */
  }
}
