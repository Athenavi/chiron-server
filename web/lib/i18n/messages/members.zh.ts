/**
 * 成员域文案（zh）。
 *
 * 覆盖：成员页（列表 / 分页 / 无权限 / 空态 / 错误）、角色与状态徽标、行操作菜单、
 * 邀请成员弹窗、二次确认与一次性密码弹窗。
 * 邮箱、角色/状态原始取值属于技术标识，保留原文；mock 示例内容不翻译。
 */
const members = {
  title: '成员',
  description: '成员与角色管理（仅所有者与管理员可见）',
  descriptionTotal: '共 {total} 名成员 · {hint}',
  quota: '套餐上限 {count} 人',
  quotaFallback: '管理可访问工作区的成员',

  actions: {
    refresh: '刷新',
    invite: '邀请成员',
    retry: '重试',
    backToChat: '返回对话',
  },

  noAccess: {
    title: '没有访问权限',
    description:
      '成员管理仅对所有者（owner）与管理员（admin）开放。如需调整角色或邀请成员，请联系工作区所有者。',
  },

  empty: {
    title: '还没有成员',
    description: '邀请第一位成员加入工作区，之后可以在这里管理角色、状态与密码。',
  },

  error: {
    loadTitle: '成员列表加载失败',
    load: '无法加载成员列表，请稍后重试',
    resetPassword: '重置密码失败，请稍后重试',
    delete: '删除成员失败，请稍后重试',
    disable: '禁用成员失败',
    enable: '启用成员失败',
    forceLogout: '强制下线失败，请稍后重试',
    invite: '创建成员失败，请稍后重试',
  },

  toast: {
    secretGenerated: '临时密码已生成',
    memberCreated: '成员已创建',
    deleted: '已删除成员 {name}',
    disabled: '已禁用 {name}',
    enabled: '已启用 {name}',
    forcedLogout: '已强制 {name} 下线',
    passwordCopied: '密码已复制',
    copyFailed: '复制失败，请手动选择密码文本',
  },

  secret: {
    resetTitle: '临时密码已生成',
    resetDescription:
      '已为 {name} 生成新的临时密码，原密码立即失效。请通过安全渠道发送，并提示对方登录后立即修改。',
    createdTitle: '成员已创建',
    createdDescription:
      '已为 {name} 创建账号。初始密码只展示这一次，请转交对方并提示首次登录修改。',
    noPassword: '（服务端未返回密码）',
    closeHint: '关闭后将无法再次查看此密码；如需重发，请对该成员重新执行「重置密码」。',
    done: '我已记下',
    copy: '复制',
    copied: '已复制',
    copyAria: '复制密码',
  },

  confirm: {
    resetTitle: '重置密码',
    resetDescription: '将为 {email} 生成新的临时密码，原密码立即失效。',
    resetConfirm: '生成临时密码',
    deleteTitle: '删除成员',
    deleteDescription: '即将删除成员 {email}。删除后该账号无法登录，也不会再出现在成员列表中。',
    deleteConfirm: '删除成员',
    irreversibleTitle: '此操作不可撤销',
    irreversibleDescription: '该成员将立即失去访问权限，其会话与数据按后端策略处理。',
    typeToConfirm: '请输入 {text} 以确认',
    mismatch: '输入内容不匹配',
  },

  role: {
    owner: '所有者',
    admin: '管理员',
    user: '成员',
    unknown: '成员',
  },

  status: {
    active: '正常',
    disabled: '已禁用',
    deleted: '已删除',
    unknown: '未知',
  },

  table: {
    caption: '成员列表，包含昵称与邮箱、角色、状态、最近登录时间以及行内操作菜单',
    mobileAria: '成员列表',
    member: '成员',
    role: '角色',
    status: '状态',
    lastLogin: '最近登录',
    actions: '操作',
    lastLoginInline: '最近登录 {time}',
    never: '从未登录',
    self: '你',
    busy: '正在处理',
  },

  skeleton: {
    aria: '正在加载成员列表…',
  },

  menu: {
    trigger: '成员操作：{name}',
    resetPassword: '重置密码',
    enable: '启用成员',
    disable: '禁用成员',
    forceLogout: '强制下线',
    forceLogoutSelf: '强制下线（自己）',
    delete: '删除成员',
    deleteSelf: '删除成员（自己）',
  },

  pagination: {
    aria: '成员分页',
    summary: '第 {page} / {totalPages} 页 · 共 {total} 名成员 · 每页 {perPage} 名',
    prev: '上一页',
    next: '下一页',
  },

  invite: {
    title: '邀请成员',
    hint: '创建账号后把一次性初始密码交给对方，首次登录需要修改密码。',
    email: '邮箱',
    emailPlaceholder: 'name@company.com',
    emailDescription: '作为登录标识，同租户内不可重复。',
    emailRequired: '请输入邮箱',
    emailInvalid: '邮箱格式不正确',
    username: '用户名（可选）',
    usernamePlaceholder: '用于 @ 提及',
    nickname: '昵称（可选）',
    nicknamePlaceholder: '显示在成员列表',
    role: '角色',
    roleHintAdmin: '管理员可以管理成员与集成配置；',
    roleHintOwnerGrant: '所有者拥有全部权限，请谨慎授予。',
    roleHintOwnerOnly: '所有者角色仅当前所有者可授予。',
    initialPassword: '初始密码（可选）',
    initialPasswordPlaceholder: '至少 8 位',
    initialPasswordDescription: '留空由服务端生成随机临时密码。',
    initialPasswordTooShort: '初始密码至少 8 位',
    errorTitle: '创建失败',
    cancel: '取消',
    submit: '创建成员',
  },
}

export default members
