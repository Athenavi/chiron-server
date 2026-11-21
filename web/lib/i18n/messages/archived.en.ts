import type zh from './archived.zh'

const en: typeof zh = {
  title: 'Archived sessions',
  description: 'Archived sessions are hidden from the chat list; restore one to keep chatting.',
  backToChat: 'Chat',
  count: 'Archived sessions: {n}',
  listLabel: 'Archived sessions',
  untitled: 'Untitled session',
  defaultModel: 'Default model',
  updatedAt: 'Updated {relative}',
  archivedAt: 'Archived {time}',
  restore: 'Restore',
  delete: 'Delete',
  loadMore: 'Load more',
  error: {
    title: 'Failed to load archived sessions',
    fallback: 'Unable to load archived sessions. Please try again later.',
    moreFallback: 'Failed to load more archived sessions. Please try again later.',
  },
  empty: {
    title: 'No archived sessions',
    body: 'Sessions you archive are moved here; restore them anytime to keep chatting.',
    go: 'Go to chat',
  },
  restoreFailed: 'Failed to restore the session. Please try again later.',
  restored: 'Restored "{title}"',
  deleteFailed: 'Failed to delete the session. Please try again later.',
  deleted: 'Deleted "{title}"',
  dialog: {
    label: 'Delete archived session',
    heading: 'Delete archived session?',
    body: '"{title}" and all of its messages will be deleted permanently. Use Restore instead if you only want to keep chatting.',
  },
  relative: {
    justNow: 'just now',
  },
  skeleton: 'Loading archived sessions…',
}

export default en
