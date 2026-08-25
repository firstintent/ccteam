/**
 * The team tree (default view): a recents strip, then sessions grouped by
 * project with delegation children indented under their parents — the same
 * workspace-grouping + expandable-catalog metaphor as DSH's own sidebar.
 * Each row: vendor monogram, title (sid fallback), activity StateDot,
 * right-aligned cost. Presentation only — every fact arrives via props.
 */
import { IconChevronDownOutline14, IconTreeCorner8x10, Pill, StateDot } from '@deepseek-ai/dsh-client-ui-primitives'
import type { TeamGraph, TeamNode } from '../shared/contract.js'
import { dotState, findNode, flattenNodes, formatCost, vendorGlyph } from './store.js'
import type { T } from './slots.js'
import css from './panel.module.css'

/** Tree view props (assembled by Panel from the one store snapshot). */
export interface SessionTreeProps {
  graph: TeamGraph
  recents: string[]
  collapsed: Record<string, boolean>
  t: T
  onOpenChat(sid: string): void
  onToggleProject(slug: string): void
}

function SessionRow({ node, depth, onOpenChat }: { node: TeamNode; depth: number; onOpenChat(sid: string): void }) {
  const cost = formatCost(node.costUsd)
  return (
    <button
      type="button"
      className={css.sessionRow}
      style={depth > 0 ? { paddingLeft: 8 + depth * 16 } : undefined}
      title={`${node.sid} · ${node.vendor}${node.model !== undefined ? ` · ${node.model}` : ''}`}
      onClick={() => {
        onOpenChat(node.sid)
      }}
    >
      {depth > 0 && <IconTreeCorner8x10 className={css.treeCorner} />}
      <span className={css.glyph} aria-hidden="true">{vendorGlyph(node.vendor)}</span>
      <span className={css.rowTitle}>{node.title ?? node.sid}</span>
      <StateDot className={css.rowDot} state={dotState(node.activity)} />
      {cost !== null && <span className={css.rowCost}>{cost}</span>}
    </button>
  )
}

/**
 * Render the team tree.
 * @param props - graph + strip/grouping state + actions.
 * @returns the tree view body.
 */
export function SessionTree({ graph, recents, collapsed, t, onOpenChat, onToggleProject }: SessionTreeProps) {
  const recentRows = recents
    .map(sid => ({ sid, node: findNode(graph, sid) }))
    .filter(row => row.node !== undefined)
  return (
    <div className={css.scroll}>
      {recentRows.length > 0 && (
        <div className={css.recents}>
          <div className={css.sectionLabel}>{t('tree.recent')}</div>
          <div className={css.recentPills}>
            {recentRows.map(({ sid, node }) => (
              <Pill
                key={sid}
                onClick={() => {
                  onOpenChat(sid)
                }}
              >
                {node?.title ?? sid}
              </Pill>
            ))}
          </div>
        </div>
      )}
      <div className={css.projects}>
        {graph.projects.map((project) => {
          const isCollapsed = collapsed[project.slug] === true
          const rows = flattenNodes(project.nodes)
          return (
            <section key={project.slug}>
              <button
                type="button"
                className={css.projectHead}
                data-collapsed={isCollapsed ? '' : undefined}
                aria-expanded={!isCollapsed}
                onClick={() => {
                  onToggleProject(project.slug)
                }}
              >
                <span className={css.projectChevron} aria-hidden="true">
                  <IconChevronDownOutline14 size={12} />
                </span>
                <span className={css.projectName}>{project.slug}</span>
                <span className={css.projectCount}>{rows.length}</span>
              </button>
              {!isCollapsed
                && rows.map(({ node, depth }) => (
                  <SessionRow key={node.sid} node={node} depth={depth} onOpenChat={onOpenChat} />
                ))}
            </section>
          )
        })}
      </div>
    </div>
  )
}
