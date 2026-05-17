// V0.5.0 F96 — Kanban Task Board panel.
//
// Three columns: Pending / In Progress / Completed. Each card carries
// title, assignee (with color avatar from the team config), dependency
// icon, timestamps. Click expands the description inline.
//
// Tasks with unknown status fold into Pending — Anthropic's host can
// drift state values; the user still sees the task. Status is the
// authoritative bucket key; dependency rendering only checks
// presence/absence, not validity.

import { memo, useMemo, useState } from "react";
import type { TeamConfig, TeamTask } from "../lib/teamsApi";
import { colorClasses } from "../lib/teamsApi";

interface Props {
  tasks: TeamTask[];
  /** Used to pick the assignee's color avatar. Members not in the
   *  team config still render — just without color. */
  config: TeamConfig | null;
}

type TaskStatusKey = "pending" | "in_progress" | "completed";

const COLUMNS: Array<{ key: TaskStatusKey; label: string }> = [
  { key: "pending", label: "Pending" },
  { key: "in_progress", label: "In Progress" },
  { key: "completed", label: "Completed" },
];

export const TaskBoard = memo(function TaskBoard({ tasks, config }: Props) {
  const buckets = useMemo(() => bucketTasks(tasks), [tasks]);
  const memberColors = useMemo(() => buildMemberColorMap(config), [config]);
  return (
    <div
      data-testid="task-board"
      className="grid grid-cols-1 md:grid-cols-3 gap-3 p-4"
    >
      {COLUMNS.map((col) => {
        const rows: TeamTask[] = buckets[col.key] ?? [];
        return (
          <section
            key={col.key}
            data-testid={`column-${col.key}`}
            className="bg-surface-800/40 rounded-lg p-2 flex flex-col gap-2 min-w-0"
          >
            <header className="flex items-center justify-between px-1">
              <h3 className="font-mono text-xs uppercase tracking-wider text-text-secondary">
                {col.label}
              </h3>
              <span className="text-[10px] text-text-dim font-mono">
                {rows.length}
              </span>
            </header>
            <div className="flex flex-col gap-2 min-h-[40px]">
              {rows.length === 0 ? (
                <p className="text-[10px] text-text-dim italic px-1">
                  no {col.label.toLowerCase()} tasks
                </p>
              ) : (
                rows.map((t) => (
                  <TaskCard
                    key={t.id}
                    task={t}
                    assigneeColor={t.assignee ? memberColors[t.assignee] ?? null : null}
                  />
                ))
              )}
            </div>
          </section>
        );
      })}
    </div>
  );
});

/** Pure helper exported so unit tests can verify the bucketing
 *  contract without rendering React. */
export function bucketTasks(tasks: TeamTask[]): {
  pending: TeamTask[];
  in_progress: TeamTask[];
  completed: TeamTask[];
} {
  const out = {
    pending: [] as TeamTask[],
    in_progress: [] as TeamTask[],
    completed: [] as TeamTask[],
  };
  for (const t of tasks) {
    if (t.status === "in_progress") out.in_progress.push(t);
    else if (t.status === "completed") out.completed.push(t);
    else out.pending.push(t);
  }
  return out;
}

/** Map teammate-name → color so TaskBoard avatars stay consistent
 *  with the Topology panel. */
export function buildMemberColorMap(
  config: TeamConfig | null,
): Record<string, string | null> {
  if (!config) return {};
  const out: Record<string, string | null> = {};
  for (const m of config.members) {
    out[m.name] = m.color ?? null;
  }
  return out;
}

interface CardProps {
  task: TeamTask;
  assigneeColor: string | null;
}

function TaskCard({ task, assigneeColor }: CardProps) {
  const [expanded, setExpanded] = useState(false);
  const colors = colorClasses(assigneeColor);
  return (
    <button
      type="button"
      onClick={() => setExpanded((v) => !v)}
      data-testid={`task-card-${task.id}`}
      className="text-left bg-surface-800/80 hover:bg-surface-800 rounded-md p-2 border border-surface-700/40 transition-colors cursor-pointer flex flex-col gap-1 min-w-0"
    >
      <div className="flex items-center gap-1 min-w-0">
        <span
          className="font-mono text-xs text-text-primary truncate flex-1"
          title={task.title}
        >
          {task.title}
        </span>
        {task.dependencies.length > 0 && (
          <span
            data-testid="dep-icon"
            title={`depends on: ${task.dependencies.join(", ")}`}
            className="text-[10px] text-text-dim font-mono shrink-0"
          >
            🔒{task.dependencies.length}
          </span>
        )}
      </div>
      {task.assignee && (
        <div className="flex items-center gap-1">
          <span
            aria-hidden
            className={`shrink-0 w-4 h-4 rounded-full flex items-center justify-center font-mono text-[9px] ${colors.bg} ${colors.text}`}
          >
            {task.assignee.slice(0, 1).toUpperCase()}
          </span>
          <span className="text-[10px] text-text-dim font-mono truncate">
            {task.assignee}
          </span>
        </div>
      )}
      <div className="flex items-center gap-1 text-[9px] text-text-dim font-mono">
        {task.created_at && <span>start: {task.created_at}</span>}
        {task.completed_at && <span>done: {task.completed_at}</span>}
      </div>
      {expanded && task.description && (
        <p
          data-testid={`task-desc-${task.id}`}
          className="text-[11px] text-text-secondary mt-1 whitespace-pre-wrap"
        >
          {task.description}
        </p>
      )}
    </button>
  );
}
