/**
 * The engine's faces, rendered to static markup with the primitives replaced
 * by plain elements: the settings 「引擎」 section per state (which buttons
 * are enabled, the stop Modal, the inert sentence), the first-run panels,
 * and the version banner per relation. No DOM — the markup is the assertion.
 */
import { describe, expect, it, vi } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'

vi.mock('@deepseek-ai/dsh-client-ui-primitives', async () => {
  const React = await import('react')
  const h = React.createElement
  const icon = () => null
  return {
    Button: ({ children, disabled, variant, icon: _icon, size: _size, onClick: _onClick, ...rest }: Record<string, unknown>) =>
      h('button', { disabled: disabled === true, 'data-variant': variant, ...(rest as object) }, children as never),
    Modal: ({ open, title, description, footer, children }: Record<string, unknown>) =>
      open === true ? h('div', { role: 'dialog', 'aria-label': title }, h('p', null, description as never), children as never, footer as never) : null,
    StateDot: ({ state }: { state: string }) => h('i', { 'data-dot': state }),
    DisclosureRow: ({ title, open, children }: Record<string, unknown>) =>
      h('div', { 'data-disclosure': title }, title as never, open === true ? (children as never) : null),
    Tooltip: ({ children }: { children: unknown }) => children,
    Input: (props: Record<string, unknown>) => h('input', props),
    Pill: ({ children }: { children: unknown }) => h('span', null, children as never),
    writeClipboard: () => {},
    IconBranchOutline16: icon,
    IconCheckOutline14: icon,
    IconCloseOutline16: icon,
    IconCodeOutline16: icon,
    IconCopyOutline16: icon,
    IconFolderOpen16: icon,
    IconLoadingOutline16: icon,
    IconPlayOutline16: icon,
    IconRightUpOutline14: icon,
    IconSettingsOutline14: icon,
    IconWarningOutline16: icon,
    IconChevronDownOutline14: icon,
  }
})

const { EngineSection } = await import('../src/client/settings/EngineSection.js')
const { EnginePanel, ProjectPanel, VersionBanner } = await import('../src/client/EnginePanel.js')
const { zh } = await import('../src/client/locales.js')
const { initialEngine } = await import('../src/client/store.js')
type EngineSlice = import('../src/client/store.js').EngineSlice
type EngineStatus = import('../src/shared/contract.js').EngineStatus
type ApiClient = import('../src/client/api.js').ApiClient
type T = import('../src/client/slots.js').T

const t = ((key: string, params?: Record<string, string | number>): string => {
  let text: string = (zh as Record<string, string>)[key] ?? key
  for (const [name, value] of Object.entries(params ?? {})) text = text.replace(`{${name}}`, String(value))
  return text
}) as unknown as T

function status(over: Partial<EngineStatus> = {}): EngineStatus {
  return {
    state: 'running',
    reachable: true,
    supervised: true,
    daemonUrl: 'http://127.0.0.1:17951',
    pinnedVersion: '0.10.3',
    home: '/tmp/sbx/home/.ccteam',
    daemonHome: '/tmp/sbx/home/.ccteam',
    binary: '/tmp/sbx/bin/ccteam',
    binarySource: 'canonical',
    binaryVersion: '0.10.3',
    runningVersion: '0.10.3',
    pid: 4242,
    webBind: '127.0.0.1:17951',
    autoStart: true,
    logPath: '/tmp/sbx/home/.ccteam/daemon.log',
    detail: 'ccteam 0.10.3 is running (pid 4242).',
    ...over,
  }
}

function slice(over: Partial<EngineSlice> = {}): EngineSlice {
  return { ...initialEngine(), ...over }
}

const api = { call: async () => ({ ok: true, path: '', lines: [] }) } as unknown as ApiClient

function section(engine: EngineSlice, autoStart = true): string {
  return renderToStaticMarkup(
    <EngineSection
      t={t}
      engine={engine}
      api={api}
      dispatch={() => {}}
      autoStart={autoStart}
      onAutoStart={() => {}}
      enginePath={{ id: 'ep', value: { text: '', overridden: false, configured: false }, writable: true, placeholder: '', onEdit: () => {}, onReset: () => {} }}
    />,
  )
}

/** Every `<button>` in the markup: its text and whether it is disabled. */
function buttons(html: string): Array<{ text: string; disabled: boolean }> {
  return [...html.matchAll(/<button([^>]*)>([^<]*)<\/button>/g)].map(match => ({
    text: match[2]!,
    disabled: /\bdisabled=""/.test(match[1]!),
  }))
}

function button(html: string, text: string) {
  const found = buttons(html).find(entry => entry.text === text)
  expect(found, `button ${text}`).toBeDefined()
  return found!
}

describe('settings engine section', () => {
  it('running: stop/restart enabled, start disabled, no update, facts + the web link', () => {
    const html = section(slice({ status: status() }))
    expect(html).toContain('运行中')
    expect(button(html, '启动').disabled).toBe(true)
    expect(button(html, '停止').disabled).toBe(false)
    expect(button(html, '重启').disabled).toBe(false)
    expect(buttons(html).some(entry => entry.text === '更新引擎')).toBe(false)
    expect(html).toContain('引擎 v0.10.3')
    expect(html).toContain('pid 4242')
    expect(html).toContain('title="/tmp/sbx/home/.ccteam"')
    expect(html).toContain('127.0.0.1:17951')
    expect(html).toContain('href="http://127.0.0.1:17951"')
    expect(html).toContain('打开 ccteam web')
    // The daemon version repeats the binary's: shown once; the host sentence repeats the facts: hidden.
    expect(html).not.toContain('daemon v0.10.3')
    expect(html).not.toContain('is running (pid 4242)')
  })

  it('stopped: only start is enabled and there is no web link', () => {
    const html = section(slice({ status: status({ state: 'stopped', reachable: false, runningVersion: undefined, pid: undefined, webBind: undefined }) }))
    expect(html).toContain('已停止')
    expect(button(html, '启动').disabled).toBe(false)
    expect(button(html, '停止').disabled).toBe(true)
    expect(button(html, '重启').disabled).toBe(true)
    expect(html).not.toContain('打开 ccteam web')
  })

  it('attached: says who started it, stop still enabled', () => {
    const html = section(slice({ status: status({ state: 'attached' }) }))
    expect(html).toContain('已挂靠')
    expect(html).toContain('由 CLI/其他入口启动,插件已接管显示')
    expect(button(html, '停止').disabled).toBe(false)
  })

  it('version mismatch with an older engine: the update button appears, the daemon version and the host sentence are shown', () => {
    const html = section(slice({ status: status({ state: 'mismatch', mismatch: 'version', runningVersion: '0.10.1', detail: 'the running engine is 0.10.1; this plugin ships against 0.10.3.' }) }))
    expect(html).toContain('版本不一致')
    expect(html).toContain('daemon v0.10.1')
    expect(html).toContain('this plugin ships against 0.10.3.')
    expect(button(html, '更新引擎').disabled).toBe(false)
    expect(button(html, '停止').disabled).toBe(true)
  })

  it('inert (pinned): the reason sentence replaces every action; status and facts remain', () => {
    const html = section(slice({ status: status({ state: 'attached', supervised: false, unsupervisedReason: 'pinned' }) }))
    expect(buttons(html)).toEqual([])
    expect(html).toContain('此配置指向由他人管理的引擎')
    expect(html).toContain('已挂靠')
    expect(html).toContain('pid 4242')
    expect(html).not.toContain('role="switch"')
    expect(html).not.toContain('引擎日志')
  })

  it('pending start: the button reads 启动中… and every action is disabled', () => {
    const html = section(slice({ status: status({ state: 'stopped', reachable: false }), pending: 'start' }))
    expect(button(html, '启动中…').disabled).toBe(true)
    expect(buttons(html).filter(entry => !entry.disabled && entry.text !== '刷新')).toEqual([])
  })

  it('stop opens the Modal (only while a confirmation is staged) with its copy', () => {
    const closed = section(slice({ status: status() }))
    expect(closed).not.toContain('role="dialog"')
    const open = section(slice({ status: status(), confirm: 'stop' }))
    expect(open).toContain('role="dialog"')
    expect(open).toContain('aria-label="停止引擎?"')
    expect(open).toContain('同时会停止 ccteam web 与 IM 网关')
    expect(buttons(open).map(entry => entry.text)).toContain('取消')
    const restart = section(slice({ status: status(), confirm: 'restart' }))
    expect(restart).toContain('aria-label="重启引擎?"')
  })

  it('shows the host error inline and the auto-start switch state', () => {
    const html = section(slice({ status: status({ state: 'stopped', reachable: false }), error: 'no platform package' }), false)
    expect(html).toContain('role="alert"')
    expect(html).toContain('操作失败:no platform package')
    expect(html).toContain('role="switch"')
    expect(html).not.toContain('checked=""')
    expect(section(slice({ status: status() }), true)).toContain('checked=""')
  })
})

function panel(engine: EngineSlice, mode: 'first-run' | 'manual' = 'first-run'): string {
  return renderToStaticMarkup(
    <EnginePanel t={t} mode={mode} status={engine.status} pending={engine.pending} error={engine.error} onStart={() => {}} />,
  )
}

describe('first-run engine panel', () => {
  it('stopped: title, reason, one-click start, and where the settings live', () => {
    const html = panel(slice({ status: status({ state: 'stopped', reachable: false, detail: 'ccteam 0.10.3 is installed at /tmp/sbx/bin/ccteam; the daemon is not running.' }) }))
    expect(html).toContain('ccteam 引擎未运行')
    expect(html).toContain('引擎已安装,但 daemon 没有运行。')
    // The host sentence repeats the reason in other words: hidden.
    expect(html).not.toContain('the daemon is not running.')
    expect(button(html, '启动引擎').disabled).toBe(false)
    expect(html).toContain('插件配置 → ccteam-ui')
  })

  it('installing / starting: a spinner title and no start button', () => {
    const html = panel(slice({ status: status({ state: 'installing', reachable: false, detail: 'installing the ccteam engine from the plugin’s platform package…' }) }))
    expect(html).toContain('正在安装引擎…')
    expect(html).not.toContain('platform package…')
    expect(buttons(html)).toEqual([])
    expect(panel(slice({ status: status({ state: 'starting', reachable: false }) }))).toContain('正在启动引擎…')
  })

  it('inert and down: the reason, no start button', () => {
    const html = panel(slice({ status: status({ state: 'stopped', reachable: false, supervised: false, unsupervisedReason: 'managed' }) }))
    expect(buttons(html)).toEqual([])
    expect(html).toContain('此 DSH 由 ccteam 启动')
  })

  it('home mismatch: both homes, the one hint, and the host sentence (it names both homes)', () => {
    const html = panel(slice({ status: status({ state: 'mismatch', mismatch: 'home', home: '/a/.ccteam', daemonHome: '/b/.ccteam', detail: 'the daemon at http://127.0.0.1:17951 runs in /b/.ccteam, not /a/.ccteam.' }) }))
    expect(html).toContain('引擎家目录不一致')
    expect(html).toContain('runs in /b/.ccteam, not /a/.ccteam.')
    expect(html).toContain('/a/.ccteam')
    expect(html).toContain('/b/.ccteam')
    expect(html).toContain('统一 CCTEAM_HOME 后重启 DSH')
    expect(buttons(html)).toEqual([])
  })

  it('unsupported: the platform title once, and a pending start reads 启动中…', () => {
    const html = panel(slice({ status: status({ state: 'unsupported', reachable: false, supervised: false, unsupervisedReason: 'unsupported' }) }))
    expect(html).toContain('不支持此平台')
    expect(html).not.toContain('什么都没有安装')
    const pending = panel(slice({ status: status({ state: 'stopped', reachable: false }), pending: 'start' }))
    expect(button(pending, '启动中…').disabled).toBe(true)
  })
})

describe('version banner', () => {
  it('engine older: the sentence and the update button (only when the action is available)', () => {
    const relation = { kind: 'engine-older' as const, engine: '0.10.1', pinned: '0.10.3' }
    const withUpdate = renderToStaticMarkup(<VersionBanner t={t} relation={relation} canUpdate pending={null} onUpdate={() => {}} onDismiss={() => {}} />)
    expect(withUpdate).toContain('role="status"')
    expect(withUpdate).toContain('引擎 v0.10.1 低于插件要求 v0.10.3')
    expect(button(withUpdate, '更新引擎').disabled).toBe(false)
    const without = renderToStaticMarkup(<VersionBanner t={t} relation={relation} canUpdate={false} pending={null} onUpdate={() => {}} onDismiss={() => {}} />)
    expect(buttons(without).some(entry => entry.text === '更新引擎')).toBe(false)
    expect(without).toContain('aria-label="关闭提示"')
  })

  it('plugin older: the sentence and the dsh plugin update command to copy; a match renders nothing', () => {
    const html = renderToStaticMarkup(
      <VersionBanner t={t} relation={{ kind: 'plugin-older', plugin: '0.10.3', engine: '0.10.9' }} canUpdate={false} pending={null} onUpdate={() => {}} onDismiss={() => {}} />,
    )
    expect(html).toContain('插件 v0.10.3 低于引擎 v0.10.9')
    // DSH refuses `dsh plugin …` without --profile; the runtime does not expose the profile name.
    expect(html).toContain('dsh plugin --profile &lt;name&gt; update @ccteam/ccteam-ui')
    expect(html).not.toContain('dsh plugin update')
    expect(html).toContain('profile = 启动 dsh web 时用的那个')
    expect(renderToStaticMarkup(<VersionBanner t={t} relation={{ kind: 'match' }} canUpdate={false} pending={null} onUpdate={() => {}} onDismiss={() => {}} />)).toBe('')
  })
})

describe('add-workspace panel', () => {
  it('renders the absolute-path input, the optional slug, and the add button', () => {
    const html = renderToStaticMarkup(<ProjectPanel t={t} busy={false} error={null} onCreate={() => {}} />)
    expect(html).toContain('添加工作区')
    expect(html).toContain('placeholder="/home/you/project"')
    expect(html).toContain('slug(可选)')
    expect(button(html, '添加').disabled).toBe(false)
    expect(html).not.toContain('从 DSH 导入')
    const busy = renderToStaticMarkup(<ProjectPanel t={t} busy error="project already exists: demo" onCreate={() => {}} />)
    expect(button(busy, '添加中…').disabled).toBe(true)
    expect(busy).toContain('添加失败:project already exists: demo')
  })

  it('offers DSH\'s own workspaces as one-click rows when the runtime hands them over', () => {
    const useWorkspaces = ((select: (state: { items: Array<{ workspaceId: string; path: string; title: string }> }) => unknown) =>
      select({ items: [{ workspaceId: 'w1', path: '/home/u/ccteam', title: 'ccteam' }, { workspaceId: 'w2', path: '/srv/site', title: 'site' }] })) as never
    const html = renderToStaticMarkup(<ProjectPanel t={t} busy={false} error={null} useWorkspaces={useWorkspaces} onCreate={() => {}} />)
    expect(html).toContain('从 DSH 导入')
    expect(html).toContain('/home/u/ccteam')
    expect(html).toContain('/srv/site')
    expect(buttons(html).filter(entry => entry.text === '添加')).toHaveLength(3)
  })
})
