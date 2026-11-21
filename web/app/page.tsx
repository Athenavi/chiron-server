import { redirect } from 'next/navigation'

/** 根路径统一进入会话列表。 */
export default function HomePage() {
  redirect('/sessions')
}
