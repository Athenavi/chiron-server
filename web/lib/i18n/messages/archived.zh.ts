const archived = {
  title: '归档会话',
  description: '已归档的会话不会出现在对话列表；恢复后可以继续对话。',
  backToChat: '对话',
  count: '共 {n} 个归档会话',
  listLabel: '归档会话列表',
  untitled: '未命名会话',
  defaultModel: '默认模型',
  updatedAt: '更新于 {relative}',
  archivedAt: '归档于 {time}',
  restore: '恢复',
  delete: '删除',
  loadMore: '加载更多',
  error: {
    title: '归档会话加载失败',
    fallback: '无法加载归档会话，请稍后重试',
    moreFallback: '加载更多归档会话失败，请稍后重试',
  },
  empty: {
    title: '还没有归档会话',
    body: '在会话里选择「归档」后，会话会移到这页；需要时随时恢复继续对话。',
    go: '去对话',
  },
  restoreFailed: '恢复会话失败，请稍后重试',
  restored: '已恢复「{title}」',
  deleteFailed: '删除会话失败，请稍后重试',
  deleted: '已删除「{title}」',
  dialog: {
    label: '删除归档会话',
    heading: '删除归档会话？',
    body: '「{title}」及其全部消息会被删除，删除后无法恢复。如果只是想继续对话，请改用「恢复」。',
  },
  relative: {
    justNow: '刚刚',
  },
  skeleton: '正在加载归档会话…',
}

export default archived
