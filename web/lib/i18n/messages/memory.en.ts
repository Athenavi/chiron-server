/**
 * Memory domain copy (en). Keys mirror `memory.zh.ts` exactly.
 */
import type zh from './memory.zh'

const en: typeof zh = {
  title: 'Memory',
  description: 'The assistant references these long-term facts in conversations.',
  count: '{count} total',
  loading: 'Loading…',

  filter: {
    aria: 'Filter memories by slot',
    listAria: 'Slot',
    all: 'All slots',
    allMemories: 'All memories',
    slot: '“{slot}” slot',
  },

  actions: {
    create: 'New memory',
    createShort: 'New',
    clear: 'Clear',
    viewAll: 'View all',
    retry: 'Retry',
  },

  list: {
    aria: 'Memory list',
    scope: 'Scope {scope}',
    source: 'Source {source}',
    confidence: 'Confidence {confidence}',
    updatedAt: 'Updated {time}',
    expand: 'Expand',
    collapse: 'Collapse',
    editAria: 'Edit “{preview}”',
    deleteAria: 'Delete “{preview}”',
  },

  skeleton: {
    aria: 'Loading memories…',
  },

  error: {
    loadTitle: 'Failed to load memories',
    loadFallback: 'Please try again later',
    load: 'Unable to load memories. Please try again later.',
    save: 'Save failed. Please try again later.',
    action: 'Action failed. Please try again later.',
  },

  empty: {
    title: 'No memories yet',
    titleFiltered: 'No memories in {target}',
    description:
      'Memories are long-term facts the assistant cites on its own, such as your role, preferences, and project details.',
  },

  toast: {
    created: 'Memory created',
    updated: 'Memory updated',
    deleted: 'Memory deleted',
    cleared: 'Cleared {count} memories',
    clearEmpty: 'Nothing to clear with the current filter',
  },

  confirm: {
    deleteTitle: 'Delete this memory?',
    deleteDescription:
      '“{preview}” will no longer be referenced by the assistant and cannot be restored.',
    clearTitle: 'Clear {target}?',
    clearDescription:
      'All {count} records under {target} will be deleted and cannot be restored.',
    delete: 'Delete',
    clear: 'Clear',
  },

  slot: {
    persona: 'Persona',
    preferences: 'Preferences',
    facts: 'Facts',
    summary: 'Summary',
    unknown: 'Other',
  },

  slotDescription: {
    persona: 'The assistant’s long-term understanding of you (one per scope)',
    preferences: 'Answer style, language, and formatting preferences',
    facts: 'Objective details such as projects and environments',
    summary: 'Session-level conversation summary; must bind to a session scope',
    unknown: 'Unknown slot',
  },

  source: {
    user: 'Manual',
    auto: 'Automatic',
    import: 'Imported',
    unknown: 'Unknown',
  },

  sourceOption: {
    user: 'Entered manually',
    auto: 'Extracted automatically',
    import: 'Imported',
  },

  scope: {
    global: 'Global',
    session: 'Session · {id}',
    unnamed: 'Unnamed',
  },

  relative: {
    justNow: 'just now',
    minutes: '{count} min ago',
    hours: '{count} hr ago',
    yesterday: 'yesterday',
  },

  form: {
    createTitle: 'New memory',
    editTitle: 'Edit memory',
    hint: 'The assistant will reference this in later conversations, so be specific.',
    slot: 'Slot',
    content: 'Content',
    contentPlaceholder: 'For example: answer in Chinese, conclusion first, then details.',
    contentDescription:
      'Plain text with line breaks; referenced in conversations as long-term information.',
    scope: 'Scope',
    scopeDescription: 'global spans sessions; session-level scopes use session:<uuid>.',
    scopeDescriptionEdit: 'The scope cannot be changed after creation.',
    scopeDescriptionSummary: 'A summary needs a session scope, for example session:0f8c…',
    source: 'Source',
    confidence: 'Confidence',
    confidencePlaceholder: '0.8 (optional)',
    confidenceDescription: 'A decimal between 0 and 1; leave empty for the default.',
    errorTitle: 'Save failed',
    errorContent: 'Memory content is required',
    errorScopeSummary: 'A summary must bind to a session scope: the scope must start with session:',
    errorScope: 'The scope must be global or session:<uuid>',
    errorConfidence: 'Confidence must be a number between 0 and 1',
    cancel: 'Cancel',
    submitCreate: 'Create memory',
    submitUpdate: 'Save changes',
  },
}

export default en
