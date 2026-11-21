/**
 * 记忆域文案（zh）。
 *
 * 覆盖：记忆页（列表 / 筛选 / 空态 / 错误 / 确认弹窗）、记忆卡片与新建/编辑弹窗。
 * 技术标识（`global`、`session:<uuid>`、`summary`、scope 原值）保留原样，只翻译
 * 其周围的说明文案；mock 示例内容不翻译。
 */
const memory = {
  title: '记忆',
  description: '助手会在对话中引用这些长期信息。',
  count: '共 {count} 条',
  loading: '加载中…',

  filter: {
    aria: '按槽位筛选记忆',
    listAria: '槽位',
    all: '全部槽位',
    allMemories: '全部记忆',
    slot: '「{slot}」槽位',
  },

  actions: {
    create: '新建记忆',
    createShort: '新建',
    clear: '清空',
    viewAll: '查看全部',
    retry: '重试',
  },

  list: {
    aria: '记忆列表',
    scope: '作用域 {scope}',
    source: '来源 {source}',
    confidence: '置信度 {confidence}',
    updatedAt: '更新于 {time}',
    expand: '展开',
    collapse: '收起',
    editAria: '编辑「{preview}」',
    deleteAria: '删除「{preview}」',
  },

  skeleton: {
    aria: '正在加载记忆…',
  },

  error: {
    loadTitle: '记忆加载失败',
    loadFallback: '请稍后重试',
    load: '无法加载记忆，请稍后重试',
    save: '保存失败，请稍后重试',
    action: '操作失败，请稍后重试',
  },

  empty: {
    title: '还没有记忆',
    titleFiltered: '{target}下还没有记忆',
    description: '记忆是助手在对话中会主动引用的长期信息，例如你的角色、偏好和项目事实。',
  },

  toast: {
    created: '记忆已创建',
    updated: '记忆已更新',
    deleted: '记忆已删除',
    cleared: '已清空 {count} 条记忆',
    clearEmpty: '当前筛选下没有可清空的记忆',
  },

  confirm: {
    deleteTitle: '删除这条记忆？',
    deleteDescription: '「{preview}」删除后助手将不再引用这条信息，且无法恢复。',
    clearTitle: '清空{target}？',
    clearDescription: '将删除{target}下的全部 {count} 条记录，删除后无法恢复。',
    delete: '删除',
    clear: '清空',
  },

  slot: {
    persona: '画像',
    preferences: '偏好',
    facts: '事实',
    summary: '摘要',
    unknown: '其他',
  },

  slotDescription: {
    persona: '助手对你的长期认知（每个作用域只保留一条）',
    preferences: '回答风格、语言与格式偏好',
    facts: '项目、环境等客观信息',
    summary: '会话级对话摘要，必须绑定 session 作用域',
    unknown: '未知槽位',
  },

  source: {
    user: '手动',
    auto: '自动',
    import: '导入',
    unknown: '未知',
  },

  sourceOption: {
    user: '手动录入',
    auto: '自动提取',
    import: '导入',
  },

  scope: {
    global: '全局',
    session: '会话 · {id}',
    unnamed: '未命名',
  },

  relative: {
    justNow: '刚刚',
    minutes: '{count} 分钟前',
    hours: '{count} 小时前',
    yesterday: '昨天',
  },

  form: {
    createTitle: '新建记忆',
    editTitle: '编辑记忆',
    hint: '这些信息会被助手在之后的对话里引用，写得越具体越有用。',
    slot: '槽位',
    content: '内容',
    contentPlaceholder: '例如：回答用中文，先给结论再展开。',
    contentDescription: '纯文本，支持换行；作为长期信息会在对话中被引用。',
    scope: '作用域',
    scopeDescription: 'global 表示跨会话；会话级写作 session:<uuid>。',
    scopeDescriptionEdit: '作用域创建后不可修改。',
    scopeDescriptionSummary: '摘要必须带会话作用域，例如 session:0f8c…',
    source: '来源',
    confidence: '置信度',
    confidencePlaceholder: '0.8（可选）',
    confidenceDescription: '0–1 之间的小数，留空表示默认。',
    errorTitle: '保存失败',
    errorContent: '记忆内容不能为空',
    errorScopeSummary: '摘要（summary）必须绑定会话作用域，scope 需以 session: 开头',
    errorScope: 'scope 只能是 global 或 session:<uuid>',
    errorConfidence: '置信度需为 0 到 1 之间的数字',
    cancel: '取消',
    submitCreate: '创建记忆',
    submitUpdate: '保存修改',
  },
}

export default memory
