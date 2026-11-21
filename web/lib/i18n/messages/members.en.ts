/**
 * Members domain copy (en). Keys mirror `members.zh.ts` exactly.
 */
import type zh from './members.zh'

const en: typeof zh = {
  title: 'Members',
  description: 'Members and roles (visible to owners and admins only)',
  descriptionTotal: '{total} members · {hint}',
  quota: 'Plan limit {count} seats',
  quotaFallback: 'Manage who can access this workspace',

  actions: {
    refresh: 'Refresh',
    invite: 'Invite member',
    retry: 'Retry',
    backToChat: 'Back to chat',
  },

  noAccess: {
    title: 'No access',
    description:
      'Member management is limited to owners and admins. Contact the workspace owner to change roles or invite members.',
  },

  empty: {
    title: 'No members yet',
    description:
      'Invite the first member to join the workspace, then manage roles, status, and passwords here.',
  },

  error: {
    loadTitle: 'Failed to load members',
    load: 'Unable to load the member list. Please try again later.',
    resetPassword: 'Failed to reset the password. Please try again later.',
    delete: 'Failed to delete the member. Please try again later.',
    disable: 'Failed to disable the member',
    enable: 'Failed to enable the member',
    forceLogout: 'Failed to force sign-out. Please try again later.',
    invite: 'Failed to create the member. Please try again later.',
  },

  toast: {
    secretGenerated: 'Temporary password generated',
    memberCreated: 'Member created',
    deleted: 'Deleted member {name}',
    disabled: 'Disabled {name}',
    enabled: 'Enabled {name}',
    forcedLogout: 'Signed out {name}',
    passwordCopied: 'Password copied',
    copyFailed: 'Copy failed. Select the password manually.',
  },

  secret: {
    resetTitle: 'Temporary password generated',
    resetDescription:
      'A new temporary password was generated for {name} and the old one is now invalid. Send it over a secure channel and ask them to change it right after signing in.',
    createdTitle: 'Member created',
    createdDescription:
      'An account was created for {name}. The initial password is shown only once; hand it over and ask them to change it on first sign-in.',
    noPassword: '(server returned no password)',
    closeHint:
      'The password cannot be viewed again after closing. To reissue it, run “Reset password” for this member again.',
    done: 'I saved it',
    copy: 'Copy',
    copied: 'Copied',
    copyAria: 'Copy password',
  },

  confirm: {
    resetTitle: 'Reset password',
    resetDescription:
      'A new temporary password will be generated for {email} and the old one becomes invalid immediately.',
    resetConfirm: 'Generate temporary password',
    deleteTitle: 'Delete member',
    deleteDescription:
      'Member {email} will be deleted. The account can no longer sign in and will not appear in the member list.',
    deleteConfirm: 'Delete member',
    irreversibleTitle: 'This cannot be undone',
    irreversibleDescription:
      'The member loses access immediately; their sessions and data are handled per the backend policy.',
    typeToConfirm: 'Type {text} to confirm',
    mismatch: 'The text does not match',
  },

  role: {
    owner: 'Owner',
    admin: 'Admin',
    user: 'Member',
    unknown: 'Member',
  },

  status: {
    active: 'Active',
    disabled: 'Disabled',
    deleted: 'Deleted',
    unknown: 'Unknown',
  },

  table: {
    caption:
      'Member list with name and email, role, status, last sign-in time, and a per-row actions menu',
    mobileAria: 'Member list',
    member: 'Member',
    role: 'Role',
    status: 'Status',
    lastLogin: 'Last sign-in',
    actions: 'Actions',
    lastLoginInline: 'Last sign-in {time}',
    never: 'Never signed in',
    self: 'You',
    busy: 'Working',
  },

  skeleton: {
    aria: 'Loading members…',
  },

  menu: {
    trigger: 'Member actions: {name}',
    resetPassword: 'Reset password',
    enable: 'Enable member',
    disable: 'Disable member',
    forceLogout: 'Force sign-out',
    forceLogoutSelf: 'Force sign-out (you)',
    delete: 'Delete member',
    deleteSelf: 'Delete member (you)',
  },

  pagination: {
    aria: 'Member pagination',
    summary: 'Page {page} / {totalPages} · {total} members · {perPage} per page',
    prev: 'Previous',
    next: 'Next',
  },

  invite: {
    title: 'Invite member',
    hint: 'After creating the account, hand over the one-time initial password; it must be changed on first sign-in.',
    email: 'Email',
    emailPlaceholder: 'name@company.com',
    emailDescription: 'Used to sign in; must be unique within the tenant.',
    emailRequired: 'Enter an email address',
    emailInvalid: 'Invalid email format',
    username: 'Username (optional)',
    usernamePlaceholder: 'Used for @ mentions',
    nickname: 'Nickname (optional)',
    nicknamePlaceholder: 'Shown in the member list',
    role: 'Role',
    roleHintAdmin: 'Admins can manage members and integration settings; ',
    roleHintOwnerGrant: 'owners have full permissions, so grant carefully.',
    roleHintOwnerOnly: 'Only the current owner can grant the owner role.',
    initialPassword: 'Initial password (optional)',
    initialPasswordPlaceholder: 'At least 8 characters',
    initialPasswordDescription: 'Leave empty to let the server generate a random temporary password.',
    initialPasswordTooShort: 'The initial password must be at least 8 characters',
    errorTitle: 'Create failed',
    cancel: 'Cancel',
    submit: 'Create member',
  },
}

export default en
