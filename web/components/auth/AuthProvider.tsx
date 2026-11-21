'use client'

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react'
import { api } from '@/lib/api'
import { useI18n } from '@/lib/i18n'
import { ApiError, type Plan, type SessionResult, type User } from '@/lib/types'

interface AuthContextValue {
  user: User | null
  plan: Plan | null
  /** 首次 `GET /auth/session` 尚未返回。 */
  loading: boolean
  error: ApiError | null
  /** 会话是否有效（不看 `must_change_password`）。 */
  authenticated: boolean
  refresh: () => Promise<void>
  /** 登录/改密后直接写入，避免多打一次 session。 */
  setAuth: (payload: Pick<SessionResult, 'user' | 'plan'> | null) => void
  logout: () => Promise<void>
}

const AuthContext = createContext<AuthContextValue | null>(null)

export function useAuth(): AuthContextValue {
  const context = useContext(AuthContext)
  if (!context) throw new Error('useAuth 必须在 AuthProvider 内使用')
  return context
}

/**
 * 认证上下文：启动时拉一次 `GET /auth/session`。
 * 路由守卫（重定向）放在 `(app)/layout.tsx` 与登录/改密页，Provider 只负责状态。
 */
export function AuthProvider({ children }: { children: ReactNode }) {
  const { t } = useI18n()
  const [user, setUser] = useState<User | null>(null)
  const [plan, setPlan] = useState<Plan | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<ApiError | null>(null)
  const loadedRef = useRef(false)

  const refresh = useCallback(async () => {
    if (!loadedRef.current) setLoading(true)
    try {
      const payload = await api.session()
      setUser(payload.user)
      setPlan(payload.plan)
      setError(null)
    } catch (cause) {
      const apiError = ApiError.from(cause, t('auth.errors.sessionCheckFailed'))
      // 401 是正常未登录，不当作错误展示
      if (apiError.status === 401) {
        setUser(null)
        setPlan(null)
        setError(null)
      } else {
        setError(apiError)
      }
    } finally {
      loadedRef.current = true
      setLoading(false)
    }
  }, [t])

  useEffect(() => {
    void refresh()
  }, [refresh])

  const setAuth = useCallback((payload: Pick<SessionResult, 'user' | 'plan'> | null) => {
    if (!payload) {
      setUser(null)
      setPlan(null)
      return
    }
    setUser(payload.user)
    setPlan(payload.plan)
    setError(null)
  }, [])

  const logout = useCallback(async () => {
    try {
      await api.logout()
    } finally {
      setUser(null)
      setPlan(null)
    }
  }, [])

  const value = useMemo<AuthContextValue>(
    () => ({
      user,
      plan,
      loading,
      error,
      authenticated: Boolean(user),
      refresh,
      setAuth,
      logout,
    }),
    [user, plan, loading, error, refresh, setAuth, logout],
  )

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>
}
