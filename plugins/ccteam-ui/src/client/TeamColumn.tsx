/**
 * The team column: the "new session" button and a search box on top (DSH's
 * own sidebar order), then sessions grouped by project with delegation
 * children indented under their parents. Each row: harness monogram, title,
 * a meta line (harness · model · when), the activity dot, the accumulated
 * cost, and — like DSH's own session rows — a trailing "⋯" menu (open,
 * rename inline, copy sid, interrupt, details, stop with a confirm). Each
 * workspace header carries DSH's ProjectRowItem affordances: hover reveals
 * a "⋯" menu (new session / copy slug / expand only / collapse all) and a
 * "+" that opens the new-session page with that project preselected.
 * Presentation only — every fact arrives via props, every action leaves
 * through a callback.
 */
import { useEffect, useRef, useState } from 'react'
import type { KeyboardEvent } from 'react'
import {
  Button,
  IconChevronDownOutline14,
  IconEllipsisOutline16,
  IconPlusOutline16,
  IconSearchOutline16,
  IconTreeCorner8x10,
  IconWarningOutline16,
  Menu,
  Modal,
  StateDot,
  Tooltip,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { MenuEntry } from '@deepseek-ai/dsh-client-ui-primitives'
import type { ProjectInfo, TeamGraph, TeamNode } from '../shared/contract.js'
import { formatCost, relativeTime, vendorGlyph } from './format.js'
import { dotState, filterRows, flattenNodes } from './store.js'
import type { T } from './slots.js'
import css from './workbench.module.css'

export interface TeamColumnProps {
  graph: TeamGraph | null
  /** The project catalog: a project with no session yet still gets a head (and its `+`). */
  projects?: ProjectInfo[] | null
  graphError: string | null
  filter: string
  collapsed: Record<string, boolean>
  selectedSid: string | null
  /** Live activity overrides from open chats (sid → activity). */
  liveActivity: Record<string, TeamNode['activity'] | undefined>
  canSpawn: boolean
  /** Fill the container (narrow pane) instead of the fixed column width. */
  fill?: boolean
  now: number
  t: T
  onSelect(sid: string): void
  onNew(): void
  onToggleProject(slug: string): void
  onProjectAction(action: ProjectAction, slug: string): void
  onFilter(filter: string): void
  onRetry(): void
  onRename(sid: string, title: string): void
  onCopySid(sid: string): void
  onInterrupt(sid: string): void
  onStop(sid: string): void
  onDetails(sid: string): void
  /** Global fold: expand every project (nothing left collapsed), or collapse every project. */
  onExpandAll(): void
  onCollapseAll(): void
}

/**
 * Relative time through the locale.
 * @param t - translate.
 * @param iso - timestamp.
 * @param now - reference time.
 * @returns the text, or null when unknown.
 */
export function whenText(t: T, iso: string | undefined, now: number): string | null {
  const bucket = relativeTime(iso, now)
  if (bucket === null) return null
  if (bucket.unit === 'now') return t('time.now')
  if (bucket.unit === 'minutes') return t('time.minutes', { value: bucket.value })
  if (bucket.unit === 'hours') return t('time.hours', { value: bucket.value })
  return t('time.days', { value: bucket.value })
}

function sumCost(nodes: readonly TeamNode[]): number {
  let total = 0
  const visit = (node: TeamNode): void => {
    total += node.costUsd ?? 0
    node.children.forEach(visit)
  }
  nodes.forEach(visit)
  return total
}

/** Row menu entry ids (the `Menu` primitive reports the picked id). */
type RowAction = 'open' | 'rename' | 'copy' | 'interrupt' | 'details' | 'stop'

/** Workspace header actions ("⋯" menu entries; "+" is `new`). */
export type ProjectAction = 'new' | 'copy' | 'solo' | 'collapseAll'

/**
 * Residency hint for a row's tooltip — only the states a reader must know
 * about (a resident session needs no caption).
 * @param t - translate.
 * @param node - the row's node.
 * @returns the caption, or null.
 */
function residencyHint(t: T, node: TeamNode): string | null {
  switch (node.residency) {
    case 'released':
      return t('row.released')
    case 'stopped':
      return t('row.stopped')
    case 'detached':
      return t('row.detached')
    default:
      return null
  }
}

/**
 * The row's dot: activity always wins; a settled (idle) session then shows
 * its residency — hollow ring = released (resumes on the next message),
 * dimmed disc = stopped, running matrix = a detached body still finishing.
 * @param activity - the resolved activity.
 * @param node - the row's node.
 * @returns the dot element.
 */
function RowDot({ activity, node }: { activity: TeamNode['activity']; node: TeamNode }) {
  if (activity === 'idle') {
    if (node.residency === 'released') return <span className={css.rowDotReleased} aria-hidden="true" />
    if (node.residency === 'stopped') return <span className={css.rowDotStopped} aria-hidden="true" />
    if (node.residency === 'detached') return <StateDot className={css.rowDot} state="ongoing" size={8} />
  }
  return <StateDot className={css.rowDot} state={dotState(activity)} size={8} />
}

function ProjectHead({ slug, collapsed, total, count, canSpawn, t, onToggle, onAction }: {
  slug: string
  collapsed: boolean
  total: string | null
  count: number
  canSpawn: boolean
  t: T
  onToggle(): void
  onAction(action: ProjectAction): void
}) {
  const [menuOpen, setMenuOpen] = useState(false)
  const items: MenuEntry[] = [
    { id: 'new', label: t('project.new'), icon: <IconPlusOutline16 size={16} />, disabled: !canSpawn },
    { id: 'copy', label: t('project.copySlug') },
    { type: 'separator', id: 'sep-1' },
    { id: 'solo', label: t('project.solo') },
    { id: 'collapseAll', label: t('project.collapseAll') },
  ]
  return (
    <div className={css.projectHead} data-menu-open={menuOpen ? '' : undefined}>
      <button
        type="button"
        className={css.projectToggle}
        data-collapsed={collapsed ? '' : undefined}
        aria-expanded={!collapsed}
        onClick={onToggle}
      >
        <span className={css.projectChevron} aria-hidden="true">
          <IconChevronDownOutline14 size={12} />
        </span>
        <span className={css.projectName}>{slug}</span>
        {total !== null && <span className={css.projectCount}>{total}</span>}
        <span className={css.projectCount}>{count}</span>
      </button>
      <span className={css.projectActions}>
        <Menu
          open={menuOpen}
          align="end"
          portal
          dense
          items={items}
          onSelect={(id) => {
            setMenuOpen(false)
            onAction(id as ProjectAction)
          }}
          onClose={() => {
            setMenuOpen(false)
          }}
          anchor={(
            <button
              type="button"
              className={css.projectIconBtn}
              aria-label={t('tree.projectActions', { slug })}
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              onClick={(event) => {
                event.stopPropagation()
                setMenuOpen(previous => !previous)
              }}
            >
              <IconEllipsisOutline16 size={16} />
            </button>
          )}
        />
        <button
          type="button"
          className={css.projectIconBtn}
          aria-label={t('tree.newIn', { slug })}
          disabled={!canSpawn}
          onClick={(event) => {
            event.stopPropagation()
            onAction('new')
          }}
        >
          <IconPlusOutline16 size={16} />
        </button>
      </span>
    </div>
  )
}

function SessionRow({ node, depth, active, activity, now, t, renaming, onSelect, onAction, onRenameCommit, onRenameCancel }: {
  node: TeamNode
  depth: number
  active: boolean
  activity: TeamNode['activity'] | undefined
  now: number
  t: T
  renaming: boolean
  onSelect(sid: string): void
  onAction(action: RowAction, node: TeamNode): void
  onRenameCommit(sid: string, title: string): void
  onRenameCancel(): void
}) {
  const [menuOpen, setMenuOpen] = useState(false)
  const [draft, setDraft] = useState(node.title ?? '')
  const inputRef = useRef<HTMLInputElement | null>(null)
  const cost = formatCost(node.costUsd)
  const when = whenText(t, node.lastActive, now)
  const rowActivity = activity ?? node.activity
  const working = rowActivity === 'working'
  const meta = [node.vendor, node.model, when, node.residency === 'stopped' ? t('residency.stopped') : undefined]
    .filter((s): s is string => s !== undefined && s !== null && s !== '')
    .join(' · ')
  const hint = residencyHint(t, node)

  useEffect(() => {
    if (!renaming) return
    setDraft(node.title ?? '')
    const timer = setTimeout(() => {
      inputRef.current?.focus()
      inputRef.current?.select()
    }, 0)
    return () => {
      clearTimeout(timer)
    }
  }, [renaming, node.title])

  const commit = (): void => {
    const next = draft.trim()
    if (next === '' || next === (node.title ?? '')) {
      onRenameCancel()
      return
    }
    onRenameCommit(node.sid, next)
  }

  const items: MenuEntry[] = [
    { id: 'open', label: t('row.open') },
    { id: 'rename', label: t('row.rename') },
    { id: 'copy', label: t('row.copySid') },
    { type: 'separator', id: 'sep-1' },
    ...(working ? [{ id: 'interrupt', label: t('row.interrupt') } as MenuEntry] : []),
    { id: 'details', label: t('row.details') },
    ...(node.residency === 'stopped'
      ? []
      : [{ type: 'separator', id: 'sep-2' } as MenuEntry, { id: 'stop', label: t('row.stop'), danger: true } as MenuEntry]),
  ]

  const onRowKey = (event: KeyboardEvent<HTMLDivElement>): void => {
    if (renaming) return
    if (event.key === 'Enter' || event.key === ' ') {
      event.preventDefault()
      onSelect(node.sid)
    }
  }

  return (
    <div
      className={css.sessionRow}
      style={depth > 0 ? { paddingLeft: 8 + depth * 14 } : undefined}
      role="button"
      tabIndex={0}
      aria-current={active ? 'true' : undefined}
      data-menu-open={menuOpen ? '' : undefined}
      data-residency={node.residency}
      title={renaming ? undefined : (hint === null ? `${node.sid} · ${meta}` : `${node.sid} · ${meta}\n${hint}`)}
      onClick={() => {
        if (!renaming) onSelect(node.sid)
      }}
      onKeyDown={onRowKey}
    >
      {depth > 0 && <IconTreeCorner8x10 className={css.treeCorner} />}
      <span className={css.glyph} aria-hidden="true">{vendorGlyph(node.vendor)}</span>
      <span className={css.rowMain}>
        {renaming
          ? (
              <input
                ref={inputRef}
                className={css.rowRename}
                value={draft}
                placeholder={t('row.rename.placeholder')}
                aria-label={t('row.rename')}
                onChange={(event) => {
                  setDraft(event.currentTarget.value)
                }}
                onClick={(event) => {
                  event.stopPropagation()
                }}
                onBlur={commit}
                onKeyDown={(event) => {
                  event.stopPropagation()
                  if (event.key === 'Enter') {
                    event.preventDefault()
                    commit()
                  } else if (event.key === 'Escape') {
                    event.preventDefault()
                    onRenameCancel()
                  }
                }}
              />
            )
          : <span className={css.rowTitle}>{node.title ?? node.sid}</span>}
        <span className={css.rowMeta}>{meta}</span>
      </span>
      <RowDot activity={rowActivity} node={node} />
      {cost !== null && <span className={css.rowCost}>{cost}</span>}
      <Menu
        open={menuOpen}
        align="end"
        portal
        dense
        items={items}
        onSelect={(id) => {
          setMenuOpen(false)
          onAction(id as RowAction, node)
        }}
        onClose={() => {
          setMenuOpen(false)
        }}
        anchor={(
          <button
            type="button"
            className={css.rowMore}
            aria-label={t('row.more')}
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            onClick={(event) => {
              event.stopPropagation()
              setMenuOpen(previous => !previous)
            }}
            onKeyDown={(event) => {
              event.stopPropagation()
            }}
          >
            <IconEllipsisOutline16 size={14} />
          </button>
        )}
      />
    </div>
  )
}

/**
 * Render the team column.
 * @param props - graph + filter/grouping state + actions.
 * @returns the column.
 */
export function TeamColumn(props: TeamColumnProps) {
  const { graph, filter, collapsed, selectedSid, liveActivity, now, t } = props
  const [renamingSid, setRenamingSid] = useState<string | null>(null)
  const [pendingStop, setPendingStop] = useState<TeamNode | null>(null)
  const graphProjects = graph?.projects ?? []
  const known = new Set(graphProjects.map(project => project.slug))
  // A project the catalog knows but the graph does not (no session yet) still
  // gets a head, so a workspace added a moment ago is visible — and spawnable
  // from its own `+` — before its first session exists.
  const projects = graph === null
    ? graphProjects
    : [
        ...graphProjects,
        ...(props.projects ?? []).filter(project => !known.has(project.slug)).map(project => ({ slug: project.slug, nodes: [] as TeamNode[] })),
      ].sort((a, b) => a.slug.localeCompare(b.slug))
  // The global fold button reflects and flips the AGGREGATE of the underlying
  // `collapsed` record, not the filtered `isCollapsed` a search narrows to —
  // a search only forces rows open for the view, it never clears the record.
  const anyCollapsed = projects.some(project => collapsed[project.slug] === true)
  const groups = projects
    .map(project => ({ slug: project.slug, rows: filterRows(flattenNodes(project.nodes), filter), total: sumCost(project.nodes) }))
    .filter(group => group.rows.length > 0 || filter.trim() === '')
  const empty = graph !== null && projects.every(project => project.nodes.length === 0)
  const noMatch = graph !== null && !empty && groups.every(group => group.rows.length === 0)

  const onAction = (action: RowAction, node: TeamNode): void => {
    switch (action) {
      case 'open':
        props.onSelect(node.sid)
        return
      case 'rename':
        setRenamingSid(node.sid)
        return
      case 'copy':
        props.onCopySid(node.sid)
        return
      case 'interrupt':
        props.onInterrupt(node.sid)
        return
      case 'details':
        props.onDetails(node.sid)
        return
      case 'stop':
        setPendingStop(node)
    }
  }

  return (
    <aside className={props.fill === true ? `${css.team} ${css.teamFill}` : css.team} aria-label={t('panel.team')}>
      <div className={css.teamTop}>
        <Button
          variant="outline"
          size="md"
          className={css.newBtn}
          icon={<IconPlusOutline16 size={16} />}
          disabled={!props.canSpawn}
          onClick={props.onNew}
        >
          {t('tree.spawn')}
        </Button>
        <label className={css.search}>
          <IconSearchOutline16 size={14} />
          <input
            className={css.searchInput}
            type="search"
            placeholder={t('tree.search')}
            value={filter}
            onChange={(event) => {
              props.onFilter(event.currentTarget.value)
            }}
          />
          <Tooltip label={anyCollapsed ? t('tree.expandAll') : t('tree.collapseAll')} delayMs={400}>
            <button
              type="button"
              className={css.foldAllBtn}
              aria-label={anyCollapsed ? t('tree.expandAll') : t('tree.collapseAll')}
              disabled={projects.length === 0}
              onClick={() => {
                if (anyCollapsed) props.onExpandAll()
                else props.onCollapseAll()
              }}
            >
              <IconChevronDownOutline14 className={css.foldAllIcon} data-collapsed={anyCollapsed ? '' : undefined} size={12} />
            </button>
          </Tooltip>
        </label>
      </div>
      <div className={css.teamList}>
        {props.graphError !== null && graph === null && (
          <div className={css.state}>
            <IconWarningOutline16 className={css.stateIcon} size={20} />
            <div className={css.stateBody}>{t('tree.error')}</div>
            <Button variant="outline" size="sm" onClick={props.onRetry}>{t('states.retry')}</Button>
          </div>
        )}
        {graph === null && props.graphError === null && (
          <div className={css.state}>
            <StateDot state="ongoing" size={12} />
          </div>
        )}
        {empty && (
          <div className={css.state}>
            <div className={css.stateTitle}>{t('tree.empty.title')}</div>
            <div className={css.stateBody}>{t('tree.empty.body')}</div>
          </div>
        )}
        {noMatch && <div className={css.centerNote}>{t('tree.noMatch')}</div>}
        {groups.map((group) => {
          const isCollapsed = collapsed[group.slug] === true && filter.trim() === ''
          const total = formatCost(group.total > 0 ? group.total : undefined)
          return (
            <section key={group.slug}>
              <ProjectHead
                slug={group.slug}
                collapsed={isCollapsed}
                total={total}
                count={group.rows.length}
                canSpawn={props.canSpawn}
                t={t}
                onToggle={() => {
                  props.onToggleProject(group.slug)
                }}
                onAction={(action) => {
                  props.onProjectAction(action, group.slug)
                }}
              />
              {!isCollapsed
                && group.rows.map(({ node, depth }) => (
                  <SessionRow
                    key={node.sid}
                    node={node}
                    depth={depth}
                    active={node.sid === selectedSid}
                    activity={liveActivity[node.sid]}
                    now={now}
                    t={t}
                    renaming={renamingSid === node.sid}
                    onSelect={props.onSelect}
                    onAction={onAction}
                    onRenameCommit={(sid, title) => {
                      setRenamingSid(null)
                      props.onRename(sid, title)
                    }}
                    onRenameCancel={() => {
                      setRenamingSid(null)
                    }}
                  />
                ))}
            </section>
          )
        })}
      </div>
      <Modal
        open={pendingStop !== null}
        title={t('stop.title')}
        description={pendingStop === null ? '' : t('stop.body', { title: pendingStop.title ?? pendingStop.sid })}
        closeLabel={t('stop.cancel')}
        onClose={() => {
          setPendingStop(null)
        }}
        footer={(
          <div className={css.actions}>
            <Button
              variant="outline"
              size="md"
              onClick={() => {
                setPendingStop(null)
              }}
            >
              {t('stop.cancel')}
            </Button>
            <Button
              variant="primary"
              size="md"
              onClick={() => {
                const node = pendingStop
                setPendingStop(null)
                if (node !== null) props.onStop(node.sid)
              }}
            >
              {t('stop.confirm')}
            </Button>
          </div>
        )}
      />
    </aside>
  )
}
