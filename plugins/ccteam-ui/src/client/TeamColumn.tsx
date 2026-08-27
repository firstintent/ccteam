/**
 * The team column: the primary "new session" button and a search box on
 * top (DSH's own sidebar order), then sessions grouped by project with
 * delegation children indented under their parents. Each row: vendor
 * monogram, title, a meta line (vendor · model · when), the activity dot
 * and the accumulated cost. Presentation only — every fact arrives via props.
 */
import {
  Button,
  IconChevronDownOutline14,
  IconPlusOutline16,
  IconSearchOutline16,
  IconTreeCorner8x10,
  IconWarningOutline16,
  StateDot,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { TeamGraph, TeamNode } from '../shared/contract.js'
import { formatCost, relativeTime, vendorGlyph } from './format.js'
import { dotState, filterRows, flattenNodes } from './store.js'
import type { T } from './slots.js'
import css from './workbench.module.css'

export interface TeamColumnProps {
  graph: TeamGraph | null
  graphError: string | null
  filter: string
  collapsed: Record<string, boolean>
  selectedSid: string | null
  /** Live activity overrides from open chats (sid → activity). */
  liveActivity: Record<string, TeamNode['activity'] | undefined>
  canSpawn: boolean
  now: number
  t: T
  onSelect(sid: string): void
  onNew(): void
  onToggleProject(slug: string): void
  onFilter(filter: string): void
  onRetry(): void
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

function SessionRow({ node, depth, active, activity, now, t, onSelect }: {
  node: TeamNode
  depth: number
  active: boolean
  activity: TeamNode['activity'] | undefined
  now: number
  t: T
  onSelect(sid: string): void
}) {
  const cost = formatCost(node.costUsd)
  const when = whenText(t, node.lastActive, now)
  const meta = [node.vendor, node.model, when].filter((s): s is string => s !== undefined && s !== null && s !== '').join(' · ')
  return (
    <button
      type="button"
      className={css.sessionRow}
      style={depth > 0 ? { paddingLeft: 8 + depth * 14 } : undefined}
      aria-current={active ? 'true' : undefined}
      title={`${node.sid} · ${meta}`}
      onClick={() => {
        onSelect(node.sid)
      }}
    >
      {depth > 0 && <IconTreeCorner8x10 className={css.treeCorner} />}
      <span className={css.glyph} aria-hidden="true">{vendorGlyph(node.vendor)}</span>
      <span className={css.rowMain}>
        <span className={css.rowTitle}>{node.title ?? node.sid}</span>
        <span className={css.rowMeta}>{meta}</span>
      </span>
      <StateDot className={css.rowDot} state={dotState(activity ?? node.activity)} size={8} />
      {cost !== null && <span className={css.rowCost}>{cost}</span>}
    </button>
  )
}

/**
 * Render the team column.
 * @param props - graph + filter/grouping state + actions.
 * @returns the column.
 */
export function TeamColumn(props: TeamColumnProps) {
  const { graph, filter, collapsed, selectedSid, liveActivity, now, t } = props
  const projects = graph?.projects ?? []
  const groups = projects
    .map(project => ({ slug: project.slug, rows: filterRows(flattenNodes(project.nodes), filter), total: sumCost(project.nodes) }))
    .filter(group => group.rows.length > 0 || filter.trim() === '')
  const empty = graph !== null && projects.every(project => project.nodes.length === 0)
  const noMatch = graph !== null && !empty && groups.every(group => group.rows.length === 0)

  return (
    <aside className={css.team} aria-label={t('panel.team')}>
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
              <button
                type="button"
                className={css.projectHead}
                data-collapsed={isCollapsed ? '' : undefined}
                aria-expanded={!isCollapsed}
                onClick={() => {
                  props.onToggleProject(group.slug)
                }}
              >
                <span className={css.projectChevron} aria-hidden="true">
                  <IconChevronDownOutline14 size={12} />
                </span>
                <span className={css.projectName}>{group.slug}</span>
                {total !== null && <span className={css.projectCount}>{total}</span>}
                <span className={css.projectCount}>{group.rows.length}</span>
              </button>
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
                    onSelect={props.onSelect}
                  />
                ))}
            </section>
          )
        })}
      </div>
    </aside>
  )
}
