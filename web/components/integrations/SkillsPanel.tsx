'use client'

/**
 * 技能面板。
 *
 * - 列表：name / version / description / source / enabled Switch（PATCH）；
 * - 「新建技能」与「编辑 manifest」共用一个弹窗，manifest 必须是 JSON 对象；
 * - 删除走确认弹窗，失败信息留在弹窗内。
 *
 * 文案全部走 i18n；manifest 解析失败的错误与文本域关联（aria-describedby），便于读屏播报。
 */

import { useCallback, useEffect, useState } from 'react'
import {
  Alert,
  Button,
  Card,
  Chip,
  Label,
  Modal,
  Switch,
  TextArea,
  toast,
} from '@heroui/react'
import { api } from '@/lib/api'
import { ApiError, type Skill } from '@/lib/types'
import { formatDateTime } from '@/lib/utils'
import { useI18n } from '@/lib/i18n'
import {
  AlertIcon,
  PencilIcon,
  PlusIcon,
  SparkIcon,
  TrashIcon,
} from '@/components/ui-bridge/icons'
import { ConfirmDialog, ListSkeleton, PanelEmpty, PanelError, parseJsonObject } from './shared'

const SKILL_TEMPLATE = `{
  "name": "my-skill",
  "version": "0.1.0",
  "description": "what this skill solves in one sentence",
  "prompt": "fixed instructions for the model",
  "resources": [],
  "scripts": []
}`

const MANIFEST_ERROR_ID = 'skill-manifest-error'

function manifestToText(manifest: Record<string, unknown>): string {
  try {
    return JSON.stringify(manifest, null, 2)
  } catch {
    return '{}'
  }
}

export interface SkillsPanelProps {
  /** 页面「刷新」按钮递增；面板据此重新拉取列表。 */
  refreshToken: number
}

type EditorMode = 'create' | 'edit'

export function SkillsPanel({ refreshToken }: SkillsPanelProps) {
  const { t, locale } = useI18n()

  const [skills, setSkills] = useState<Skill[]>([])
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)

  const [togglingId, setTogglingId] = useState<string | null>(null)
  const [deletingId, setDeletingId] = useState<string | null>(null)
  const [deleteError, setDeleteError] = useState<string | null>(null)
  const [pendingDelete, setPendingDelete] = useState<Skill | null>(null)

  // 新建 / 编辑 manifest
  const [mode, setMode] = useState<EditorMode | null>(null)
  const [editing, setEditing] = useState<Skill | null>(null)
  const [manifestText, setManifestText] = useState('')
  const [enabledDraft, setEnabledDraft] = useState(true)
  const [manifestError, setManifestError] = useState<string | null>(null)
  const [formError, setFormError] = useState<string | null>(null)
  const [submitting, setSubmitting] = useState(false)

  const load = useCallback(async () => {
    setLoading(true)
    setError(null)
    try {
      setSkills(await api.listSkills())
    } catch (cause) {
      setError(ApiError.from(cause, t('integrations.skills.loadError')).message)
    } finally {
      setLoading(false)
    }
  }, [t])

  useEffect(() => {
    void load()
  }, [load, refreshToken])

  function openCreate() {
    setMode('create')
    setEditing(null)
    setManifestText(SKILL_TEMPLATE)
    setEnabledDraft(true)
    setManifestError(null)
    setFormError(null)
    setSubmitting(false)
  }

  function openEdit(skill: Skill) {
    setMode('edit')
    setEditing(skill)
    setManifestText(manifestToText(skill.manifest))
    setEnabledDraft(skill.enabled)
    setManifestError(null)
    setFormError(null)
    setSubmitting(false)
  }

  async function handleToggle(skill: Skill, next: boolean) {
    if (togglingId) return
    setTogglingId(skill.id)
    setSkills((prev) =>
      prev.map((item) => (item.id === skill.id ? { ...item, enabled: next } : item)),
    )
    try {
      const updated = await api.updateSkill(skill.id, { enabled: next })
      setSkills((prev) => prev.map((item) => (item.id === updated.id ? updated : item)))
      toast.success(
        next
          ? t('integrations.shared.enabledToast', { name: updated.name })
          : t('integrations.shared.disabledToast', { name: updated.name }),
      )
    } catch (cause) {
      setSkills((prev) =>
        prev.map((item) => (item.id === skill.id ? { ...item, enabled: skill.enabled } : item)),
      )
      toast.danger(ApiError.from(cause, t('integrations.skills.toggleError')).message)
    } finally {
      setTogglingId(null)
    }
  }

  async function handleDelete() {
    const target = pendingDelete
    if (!target || deletingId) return
    setDeletingId(target.id)
    setDeleteError(null)
    try {
      await api.deleteSkill(target.id)
      setSkills((prev) => prev.filter((item) => item.id !== target.id))
      setPendingDelete(null)
      toast.success(t('integrations.shared.deletedToast', { name: target.name }))
    } catch (cause) {
      setDeleteError(ApiError.from(cause, t('integrations.skills.deleteError')).message)
    } finally {
      setDeletingId(null)
    }
  }

  async function handleSubmit(event: React.FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (submitting || !mode) return

    const manifest = parseJsonObject(manifestText, 'manifest', t)
    if (!manifest.ok) {
      setManifestError(manifest.error)
      return
    }
    if (mode === 'create') {
      const manifestName = manifest.value.name
      if (typeof manifestName !== 'string' || !manifestName.trim()) {
        setManifestError(t('integrations.skills.manifestNameRequired'))
        return
      }
    }
    setManifestError(null)
    setFormError(null)

    setSubmitting(true)
    try {
      if (mode === 'create') {
        const created = await api.createSkill({ manifest: manifest.value, enabled: enabledDraft })
        toast.success(t('integrations.skills.createdToast', { name: created.name }))
      } else if (editing) {
        const updated = await api.updateSkill(editing.id, { manifest: manifest.value })
        toast.success(t('integrations.skills.updatedToast', { name: updated.name }))
      }
      setMode(null)
      setEditing(null)
      await load()
    } catch (cause) {
      const fallback =
        mode === 'create'
          ? t('integrations.skills.createError')
          : t('integrations.skills.saveError')
      setFormError(ApiError.from(cause, fallback).message)
    } finally {
      setSubmitting(false)
    }
  }

  const editorOpen = mode !== null
  const editorTitle =
    mode === 'edit' ? t('integrations.skills.editTitle') : t('integrations.skills.createTitle')
  const submittingLabel =
    mode === 'edit' ? t('common.save') : t('integrations.skills.submit')

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <p className="max-w-prose text-[13px] leading-6 text-muted">
          {t('integrations.skills.intro')}
        </p>
        <Button
          variant="primary"
          size="sm"
          className="h-10 gap-1.5 md:h-9"
          onPress={openCreate}
          isDisabled={loading}
        >
          <PlusIcon size={15} aria-hidden="true" />
          {t('integrations.skills.create')}
        </Button>
      </div>

      {loading ? <ListSkeleton /> : null}

      {!loading && error ? <PanelError message={error} onRetry={() => void load()} /> : null}

      {!loading && !error && skills.length === 0 ? (
        <PanelEmpty
          icon={<SparkIcon size={20} />}
          title={t('integrations.skills.emptyTitle')}
          description={t('integrations.skills.emptyDescription')}
          action={
            <Button variant="primary" className="h-10 gap-1.5 px-4" onPress={openCreate}>
              <PlusIcon size={15} aria-hidden="true" />
              {t('integrations.skills.create')}
            </Button>
          }
        />
      ) : null}

      {!loading && !error && skills.length > 0 ? (
        <ul className="grid grid-cols-1 gap-3 lg:grid-cols-2">
          {skills.map((skill) => (
            <li key={skill.id}>
              <Card className="h-full">
                <Card.Header className="flex-row items-start gap-3">
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <Card.Title className="truncate font-mono text-sm">{skill.name}</Card.Title>
                      <Chip size="sm" variant="soft" color="default">
                        v{skill.version}
                      </Chip>
                      <Chip size="sm" variant="soft" color="default">
                        {skill.source}
                      </Chip>
                      {skill.enabled ? null : (
                        <Chip size="sm" variant="soft" color="danger">
                          {t('integrations.shared.disabledBadge')}
                        </Chip>
                      )}
                    </div>
                    <Card.Description className="mt-1 text-[12px] leading-5 break-words">
                      {skill.description || t('integrations.shared.noDescription')}
                    </Card.Description>
                  </div>
                  <Switch
                    size="sm"
                    className="shrink-0"
                    isSelected={skill.enabled}
                    isDisabled={togglingId === skill.id}
                    aria-label={t('integrations.shared.enableAria', { name: skill.name })}
                    onChange={(next) => void handleToggle(skill, next)}
                  >
                    <Switch.Content className="min-h-10">
                      <Switch.Control>
                        <Switch.Thumb />
                      </Switch.Control>
                    </Switch.Content>
                  </Switch>
                </Card.Header>

                <Card.Content className="flex flex-col gap-3">
                  <p className="text-[11px] leading-5 text-muted">
                    {t('integrations.shared.updatedAt', {
                      time: formatDateTime(skill.updated_at, locale),
                    })}
                  </p>
                  <div className="flex flex-wrap items-center gap-2">
                    <Button
                      variant="secondary"
                      size="sm"
                      className="h-10 gap-1.5 md:h-8"
                      onPress={() => openEdit(skill)}
                    >
                      <PencilIcon size={13} aria-hidden="true" />
                      {t('integrations.skills.editTitle')}
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      className="h-10 gap-1.5 text-danger md:h-8"
                      onPress={() => {
                        setDeleteError(null)
                        setPendingDelete(skill)
                      }}
                    >
                      <TrashIcon size={13} aria-hidden="true" />
                      {t('common.delete')}
                    </Button>
                  </div>
                </Card.Content>
              </Card>
            </li>
          ))}
        </ul>
      ) : null}

      {/* 新建 / 编辑 manifest */}
      <Modal
        isOpen={editorOpen}
        onOpenChange={(open) => {
          if (submitting) return
          if (!open) {
            setMode(null)
            setEditing(null)
          }
        }}
      >
        <Modal.Backdrop isDismissable={!submitting}>
          <Modal.Container placement="center" scroll="inside">
            <Modal.Dialog aria-label={editorTitle} className="w-full max-w-lg">
              <Modal.Header>
                <Modal.Icon className="bg-accent-soft text-accent">
                  <SparkIcon size={18} aria-hidden="true" />
                </Modal.Icon>
                <div className="min-w-0 flex-1">
                  <Modal.Heading>{editorTitle}</Modal.Heading>
                  <p className="mt-0.5 text-xs leading-5 text-muted">
                    {mode === 'edit'
                      ? t('integrations.skills.editDescription', { name: editing?.name ?? '' })
                      : t('integrations.skills.createDescription')}
                  </p>
                </div>
              </Modal.Header>

              <Modal.Body>
                <form
                  id="skill-editor-form"
                  className="flex flex-col gap-4"
                  noValidate
                  onSubmit={(event) => void handleSubmit(event)}
                >
                  <div className="flex flex-col gap-1.5">
                    <p className="text-[11px] font-medium tracking-wide text-subtle uppercase">
                      {t('integrations.skills.manifestLabel')}
                    </p>
                    <TextArea
                      rows={12}
                      aria-label={t('integrations.skills.manifestAria')}
                      aria-invalid={manifestError ? true : undefined}
                      aria-describedby={manifestError ? MANIFEST_ERROR_ID : undefined}
                      value={manifestText}
                      placeholder={SKILL_TEMPLATE}
                      onChange={(event) => {
                        setManifestText(event.target.value)
                        if (manifestError) setManifestError(null)
                      }}
                      disabled={submitting}
                      className="min-h-[240px] resize-y font-mono text-[12px]"
                    />
                  </div>
                  {manifestError ? (
                    <p
                      id={MANIFEST_ERROR_ID}
                      role="alert"
                      className="text-[12px] leading-5 break-all text-danger"
                    >
                      {manifestError}
                    </p>
                  ) : null}

                  {mode === 'create' ? (
                    <Switch
                      isSelected={enabledDraft}
                      isDisabled={submitting}
                      onChange={setEnabledDraft}
                    >
                      <Switch.Content className="min-h-10">
                        <Switch.Control>
                          <Switch.Thumb />
                        </Switch.Control>
                        <Label>{t('integrations.shared.enableAfterCreate')}</Label>
                      </Switch.Content>
                    </Switch>
                  ) : null}

                  {formError ? (
                    <Alert status="danger">
                      <Alert.Indicator>
                        <AlertIcon aria-hidden="true" className="size-4" />
                      </Alert.Indicator>
                      <Alert.Content>
                        <Alert.Title>
                          {mode === 'edit'
                            ? t('integrations.skills.formSaveErrorTitle')
                            : t('integrations.skills.formCreateErrorTitle')}
                        </Alert.Title>
                        <Alert.Description className="break-all">{formError}</Alert.Description>
                      </Alert.Content>
                    </Alert>
                  ) : null}
                </form>
              </Modal.Body>

              <Modal.Footer>
                <Button
                  variant="ghost"
                  className="h-10 md:h-9"
                  isDisabled={submitting}
                  onPress={() => setMode(null)}
                >
                  {t('common.cancel')}
                </Button>
                <Button
                  type="submit"
                  form="skill-editor-form"
                  variant="primary"
                  className="h-10 md:h-9"
                  isPending={submitting}
                >
                  {submittingLabel}
                </Button>
              </Modal.Footer>
            </Modal.Dialog>
          </Modal.Container>
        </Modal.Backdrop>
      </Modal>

      <ConfirmDialog
        open={pendingDelete !== null}
        title={t('integrations.skills.deleteTitle')}
        description={t('integrations.skills.deleteDescription', {
          name: pendingDelete?.name ?? '',
        })}
        confirmLabel={t('common.delete')}
        isPending={deletingId !== null}
        error={deleteError}
        onOpenChange={(open) => {
          if (!open) {
            setPendingDelete(null)
            setDeleteError(null)
          }
        }}
        onConfirm={() => void handleDelete()}
      />
    </div>
  )
}
