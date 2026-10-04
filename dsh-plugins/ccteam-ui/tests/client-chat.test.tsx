/**
 * The chat view over one long turn, rendered to static markup with the
 * primitives replaced by plain elements: what the session says mid-turn shows
 * at once while the working row and Stop stay up, and the status-only frame
 * that closes the turn takes both down without leaving a row of its own.
 */
import { describe, expect, it, vi } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'

vi.mock('@deepseek-ai/dsh-client-ui-primitives', async () => {
  const React = await import('react')
  const h = React.createElement
  const icon = () => null
  return {
    Button: ({ children, disabled }: Record<string, unknown>) => h('button', { disabled: disabled === true }, children as never),
    MarkdownText: ({ text }: { text: string }) => h('p', { 'data-md': '' }, text),
    Menu: ({ anchor }: { anchor: unknown }) => anchor as never,
    StateDot: ({ state }: { state: string }) => h('i', { 'data-dot': state }),
    IconApiOutline14: icon,
    IconChevronDownOutline14: icon,
    IconCloseFill14: icon,
    IconCodeOutline16: icon,
    IconEditOutline16: icon,
    IconGlobeOutline14: icon,
    IconLoadingOutline16: icon,
    IconPaperclipOutline16: icon,
    IconSendOutline16: icon,
    IconSparkle16: icon,
    IconStopFill16: icon,
    IconThinkOutline16: icon,
  }
})

const { Chat } = await import('../src/client/Chat.js')
const { zh } = await import('../src/client/locales.js')
const { chatOf, initialState, reduce } = await import('../src/client/store.js')
type Action = import('../src/client/store.js').Action
type ChatState = import('../src/client/store.js').ChatState
type ApiClient = import('../src/client/api.js').ApiClient
type SessionEvent = import('../src/shared/contract.js').SessionEvent
type T = import('../src/client/slots.js').T

const t = ((key: string, params?: Record<string, string | number>): string => {
  let text: string = (zh as Record<string, string>)[key] ?? key
  for (const [name, value] of Object.entries(params ?? {})) text = text.replace(`{${name}}`, String(value))
  return text
}) as unknown as T

const NOW = 1_700_000_000_000
const api = {} as unknown as ApiClient
const on = (event: SessionEvent): Action => ({ type: 'session_event', sid: 's1', event, now: NOW })

function chatAfter(actions: Action[]): ChatState {
  return chatOf(actions.reduce(reduce, initialState()), 's1')
}

function render(chat: ChatState): string {
  return renderToStaticMarkup(
    <Chat
      sid="s1"
      project="p"
      chat={chat}
      node={undefined}
      models={null}
      selectedStep={null}
      api={api}
      dispatch={() => {}}
      t={t}
      onSelectStep={() => {}}
    />,
  )
}

/** Assistant blocks in the transcript (settled rows and the live steps block). */
function assistantBlocks(html: string): number {
  return [...html.matchAll(/<div class="[^"]*turnAssistant[^"]*">/g)].length
}

const MID_TURN: Action[] = [
  { type: 'send_started', sid: 's1', text: 'fix the build' },
  on({ kind: 'activity', step: { itemId: 't1', kind: 'command_exec', name: 'cargo', summary: 'cargo build', status: 'started' } }),
  on({ kind: 'activity', step: { itemId: 't1', kind: 'command_exec', name: 'cargo', summary: 'cargo build', status: 'completed' } }),
  on({ kind: 'answer', id: 'a1', content: 'The build fails in the linker; patching the flags next.', interim: true }),
]

describe('chat: a long turn', () => {
  it('an interim answer renders at once and the working row and Stop stay up', () => {
    const html = render(chatAfter(MID_TURN))
    expect(html).toContain('The build fails in the linker; patching the flags next.')
    expect(html).toContain(zh['chat.working'])
    expect(html).toContain('data-stop=""')
    expect(assistantBlocks(html)).toBe(1)
  })

  it('the status-only closing frame ends the working state and adds no row', () => {
    const html = render(chatAfter([
      ...MID_TURN,
      on({ kind: 'answer', id: 'close', content: '', status: { turn: 4, costUsd: 0.3 } }),
    ]))
    expect(html).toContain('The build fails in the linker; patching the flags next.')
    expect(html).not.toContain(zh['chat.working'])
    expect(html).not.toContain('data-stop=""')
    // One assistant block (the interim answer, with its step) and no empty one.
    expect(assistantBlocks(html)).toBe(1)
    expect(html).not.toMatch(/<div class="[^"]*turnAssistant[^"]*"><\/div>/)
  })

  it('a turn-ending answer with no status (the model switch receipt) takes Stop down', () => {
    const html = render(chatAfter([
      { type: 'send_started', sid: 's1', text: '/model opus' },
      on({ kind: 'progress', content: '', done: false }),
      on({ kind: 'answer', id: 'r1', content: 'switched model → opus' }),
    ]))
    expect(html).toContain('switched model → opus')
    expect(html).not.toContain(zh['chat.working'])
    expect(html).not.toContain('data-stop=""')
  })
})
