import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { EV_OS_DRAG_DROP, EV_OS_DRAG_ENTER, EV_OS_DRAG_LEAVE } from '../ipc/real';
import { ipc } from '../ipc/ipc';
import { resetAllMockIpc } from '../ipc/mock';
import { SessionProvider, useSession } from '../state/session';
import AppShell from './AppShell';
import FilePanel from './FilePanel';
import { ChangelogSection } from './PluginManagerPage';
import type { ChangelogEntry } from '../ipc/types';
/** Mocked Tauri bridge for the real-mode suite below (invoke / event layer / dialog plugin). */
const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
  open: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (channel: string, handler: (event: { payload: unknown }) => void) => {
    const prev = tauri.listeners.get(channel);
    tauri.listeners.set(channel, handler);
    return () => {
      if (tauri.listeners.get(channel) === handler) {
        if (prev) tauri.listeners.set(channel, prev);
        else tauri.listeners.delete(channel);
      }
    };
  }),
}));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: tauri.open }));

/** Test harness: simulate a plugin crash via a health event (mock never crashes on its own). */
function CrashPlugin({ pluginId }: { pluginId: string }) {
  const { dispatch } = useSession();
  return (
    <button
      type="button"
      onClick={() =>
        dispatch({
          type: 'plugins/health',
          payload: { plugin_id: pluginId, state: 'crashed', prev_state: 'ready', detail: 'exit code 42' },
        })
      }
    >
      crash
    </button>
  );
}

function renderPage() {
  window.location.hash = '#/plugins';
  return render(
    <SessionProvider>
      <CrashPlugin pluginId="builtin-csv" />
      <FilePanel />
      <AppShell />
    </SessionProvider>,
  );
}

async function advance(ms: number): Promise<void> {
  const step = 250;
  for (let t = 0; t < ms; t += step) {
    await act(async () => {
      await vi.advanceTimersByTimeAsync(Math.min(step, ms - t));
    });
  }
}

async function importPath(path: string): Promise<void> {
  fireEvent.change(screen.getByTestId('path-input'), { target: { value: path } });
  fireEvent.click(screen.getByRole('button', { name: 'Import Files' }));
  await advance(500);
}

function rowById(id: string): HTMLElement {
  const row = screen.getAllByTestId('plugin-row').find((r) => r.getAttribute('data-plugin-id') === id);
  if (!row) throw new Error(`plugin row ${id} not found`);
  return row;
}

function badgeOf(id: string): HTMLElement {
  return within(rowById(id)).getByTestId('plugin-badge');
}

/** The innermost span carrying the line text; its parent .plugin-log__line holds the level class. */
function logLineFor(text: RegExp): HTMLElement {
  const line = screen.getByText(text).closest('.plugin-log__line');
  if (!line) throw new Error(`log line ${text} not found`);
  return line as HTMLElement;
}

describe('PluginManagerPage (ipc-ui.md §4.6)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    resetAllMockIpc();
    window.location.hash = '';
  });

  it('lists discovered plugins with ready badges, versions and capabilities', async () => {
    renderPage();
    await advance(500);

    expect(screen.getAllByTestId('plugin-row')).toHaveLength(2);
    expect(within(rowById('builtin-csv')).getByText('v1.0.0')).toBeInTheDocument();
    expect(badgeOf('builtin-csv')).toHaveTextContent('Ready');
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'ready');

    fireEvent.click(within(rowById('builtin-csv')).getByTestId('drawer-toggle'));
    const drawer = within(rowById('builtin-csv')).getByTestId('plugin-drawer');
    expect(drawer).toHaveTextContent('Annotate: Off');
    expect(drawer).toHaveTextContent('Subscribe: Off');
    expect(drawer).toHaveTextContent('Binary sidecar: Off');

    fireEvent.click(within(rowById('demo-tool')).getByTestId('drawer-toggle'));
    const drawer2 = within(rowById('demo-tool')).getByTestId('plugin-drawer');
    expect(drawer2).toHaveTextContent('Annotate: On');
    expect(drawer2).toHaveTextContent('Subscribe: On');
  });

  it('flips the badge live with ab://plugin-health through the import pipeline', async () => {
    renderPage();
    await advance(500);
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'ready');

    await importPath('C:\\data\\p.csv');
    await advance(200);
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'parsing');
    expect(badgeOf('builtin-csv')).toHaveTextContent('Parsing');
    expect(badgeOf('demo-tool')).toHaveAttribute('data-state', 'ready');

    await advance(10_000);
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'ready');
    expect(badgeOf('builtin-csv')).toHaveTextContent('Ready');
  });

  it('shows last_error on crashed and reload flips the badge back to ready', async () => {
    const spy = vi.spyOn(ipc, 'reload_plugin');
    renderPage();
    await advance(500);

    fireEvent.click(screen.getByRole('button', { name: 'crash' }));
    expect(badgeOf('builtin-csv')).toHaveTextContent('Crashed');
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'crashed');
    expect(within(rowById('builtin-csv')).getByTestId('plugin-last-error')).toHaveTextContent(
      'Last error: exit code 42',
    );

    fireEvent.click(within(rowById('builtin-csv')).getByTestId('reload-btn'));
    await advance(600);
    expect(spy).toHaveBeenCalledWith({ plugin_id: 'builtin-csv' });
    expect(badgeOf('builtin-csv')).toHaveTextContent('Ready');
    expect(badgeOf('builtin-csv')).toHaveAttribute('data-state', 'ready');
    expect(within(rowById('builtin-csv')).queryByTestId('plugin-last-error')).not.toBeInTheDocument();
  });

  it('drawer backfills history via get_plugin_log on open and appends live events with level coloring', async () => {
    const spy = vi.spyOn(ipc, 'get_plugin_log');
    renderPage();
    await advance(500);

    await importPath('C:\\data\\logs.dat');
    await advance(500);

    fireEvent.click(within(rowById('demo-tool')).getByTestId('drawer-toggle'));
    await advance(500);
    expect(spy).toHaveBeenCalledWith({ plugin_id: 'demo-tool' });

    const scroller = screen.getByTestId('log-scroller');
    expect(scroller).toHaveTextContent('starting');
    expect(scroller).toHaveTextContent('handshake');
    expect(logLineFor(/locale config missing/).className).toContain('plugin-log__line--error');
    expect(scroller.querySelectorAll('.plugin-log__line').length).toBeGreaterThanOrEqual(3);

    await advance(10_000);
    expect(logLineFor(/slow batch/).className).toContain('plugin-log__line--warn');
    expect(scroller.querySelectorAll('.plugin-log__line').length).toBeGreaterThanOrEqual(5);
  });

  it('auto-scrolls to the bottom while following; up-scroll pauses and the follow button resumes', async () => {
    renderPage();
    await advance(500);
    await importPath('C:\\data\\logs.dat');
    await advance(500);

    fireEvent.click(within(rowById('demo-tool')).getByTestId('drawer-toggle'));
    await advance(500);
    const scroller = screen.getByTestId('log-scroller') as HTMLDivElement;
    expect(scroller.getAttribute('data-following')).toBe('true');

    scroller.scrollTop = 123;
    fireEvent.scroll(scroller);
    expect(scroller.getAttribute('data-following')).toBe('true');

    await advance(10_000);
    expect(screen.getByText(/slow batch/)).toBeInTheDocument();
    expect(scroller.scrollTop).toBe(0);

    scroller.scrollTop = 50;
    fireEvent.scroll(scroller);
    expect(scroller.getAttribute('data-following')).toBe('false');

    fireEvent.click(within(rowById('demo-tool')).getByTestId('reload-btn'));
    await advance(500);
    expect(screen.getByText(/reloaded/)).toBeInTheDocument();
    expect(scroller.scrollTop).toBe(50);

    fireEvent.click(screen.getByTestId('follow-btn'));
    expect(scroller.getAttribute('data-following')).toBe('true');
    expect(scroller.scrollTop).toBe(0);
  });

  it('drawer shows the loaded file ids of a ready plugin', async () => {
    renderPage();
    await advance(500);
    await importPath('C:\\data\\f.csv');
    await advance(10_000);

    fireEvent.click(within(rowById('builtin-csv')).getByTestId('drawer-toggle'));
    await advance(500);
    const files = within(rowById('builtin-csv')).getByTestId('loaded-files');
    expect(files).toHaveTextContent(/mock-f-/);
    expect(files).not.toHaveTextContent('—');
  });
});

/** Mock-mode module manager flows (spec §6, task 7). */
describe('PluginManagerPage module manager (spec §6)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
    resetAllMockIpc();
    window.location.hash = '';
  });

  async function dropZip(name: string): Promise<void> {
    fireEvent.drop(screen.getByTestId('plugin-dropzone'), {
      dataTransfer: { files: [new File([''], name)] },
    });
    await advance(500);
  }

  async function installFixture(): Promise<void> {
    renderPage();
    await advance(500);
    await dropZip('fixture.zip');
  }

  it('builtin rows show the ships-with-app marker and hide the uninstall button', async () => {
    renderPage();
    await advance(500);

    const builtin = rowById('builtin-csv');
    expect(within(builtin).getByTestId('builtin-marker')).toHaveTextContent('Ships with the app');
    expect(within(builtin).queryByTestId('uninstall-plugin-btn')).not.toBeInTheDocument();

    const demo = rowById('demo-tool');
    expect(within(demo).queryByTestId('builtin-marker')).not.toBeInTheDocument();
    expect(within(demo).getByTestId('uninstall-plugin-btn')).toBeInTheDocument();
  });

  it('the disable toggle flips the row to disabled and shows 启用', async () => {
    const spy = vi.spyOn(ipc, 'set_plugin_enabled');
    renderPage();
    await advance(500);

    fireEvent.click(within(rowById('demo-tool')).getByTestId('toggle-enabled-btn'));
    await advance(500);
    expect(spy).toHaveBeenCalledWith({ plugin_id: 'demo-tool', enabled: false });
    expect(rowById('demo-tool').className).toContain('plugin-row--disabled');
    expect(within(rowById('demo-tool')).getByTestId('disabled-marker')).toBeInTheDocument();
    expect(within(rowById('demo-tool')).getByTestId('toggle-enabled-btn')).toHaveTextContent('Enable');

    fireEvent.click(within(rowById('demo-tool')).getByTestId('toggle-enabled-btn'));
    await advance(500);
    expect(rowById('demo-tool').className).not.toContain('plugin-row--disabled');
    expect(within(rowById('demo-tool')).getByTestId('toggle-enabled-btn')).toHaveTextContent('Disable');
  });

  it('uninstall removes the row and is not offered for builtin plugins', async () => {
    const spy = vi.spyOn(ipc, 'uninstall_plugin');
    renderPage();
    await advance(500);

    fireEvent.click(within(rowById('demo-tool')).getByTestId('uninstall-plugin-btn'));
    await advance(500);
    expect(spy).toHaveBeenCalledWith({ plugin_id: 'demo-tool' });
    expect(screen.queryAllByTestId('plugin-row')).toHaveLength(1);
    expect(rowById('builtin-csv')).toBeInTheDocument();
  });

  it('dropzone installs a zip drop as a new plugin row (HTML5 fallback, mock mode)', async () => {
    const spy = vi.spyOn(ipc, 'install_plugin_zip');
    renderPage();
    await advance(500);

    await dropZip('fixture.zip');
    expect(spy).toHaveBeenCalledWith({ path: 'fixture.zip', overwrite: false });
    expect(rowById('fixture-csv')).toBeInTheDocument();
    expect(within(rowById('fixture-csv')).getByText('v1.1.0')).toBeInTheDocument();
  });

  it('non-zip drops are ignored', async () => {
    const spy = vi.spyOn(ipc, 'install_plugin_zip');
    renderPage();
    await advance(500);

    await dropZip('readme.txt');
    expect(spy).not.toHaveBeenCalled();
    expect(screen.queryAllByTestId('plugin-row')).toHaveLength(2);
  });

  it('a bad zip shows the install error banner', async () => {
    renderPage();
    await advance(500);

    await dropZip('bad.zip');
    const banner = screen.getByTestId('plugin-page-error');
    expect(banner).toHaveTextContent('Install failed');
    expect(banner).toHaveTextContent('Module install failed');
  });

  it('a conflicting install asks for overwrite confirmation and overwrite succeeds', async () => {
    renderPage();
    await advance(500);

    await dropZip('conflict.zip');
    const confirm = screen.getByTestId('install-conflict');
    expect(confirm).toHaveTextContent('Version v1.1.0 is already installed. Overwrite it?');

    fireEvent.click(within(confirm).getByTestId('install-overwrite-btn'));
    await advance(500);
    expect(rowById('fixture-csv')).toBeInTheDocument();
    expect(screen.queryByTestId('install-conflict')).not.toBeInTheDocument();
  });

  it('a same-version conflict shows an informational notice without the overwrite button', async () => {
    renderPage();
    await advance(500);

    await dropZip('same.zip');
    const notice = screen.getByTestId('install-same-version');
    expect(notice).toHaveTextContent('Version v1.1.0 is already installed');
    expect(screen.queryByTestId('install-overwrite-btn')).not.toBeInTheDocument();
    expect(screen.queryByTestId('install-conflict')).not.toBeInTheDocument();
  });

  it('P2-03: discovered rows show installed-pending copy, a verify hint and a verify button instead of reload', async () => {
    await installFixture();

    const row = rowById('fixture-csv');
    expect(badgeOf('fixture-csv')).toHaveTextContent('已安装，等待首次运行验证');
    expect(badgeOf('fixture-csv')).toHaveAttribute('data-state', 'discovered');
    expect(within(row).getByTestId('verify-hint')).toHaveTextContent(/首次导入匹配该模块的日志文件/);
    expect(within(row).getByTestId('verify-plugin-btn')).toHaveTextContent('验证模块');
    expect(within(row).queryByTestId('reload-btn')).not.toBeInTheDocument();

    expect(within(rowById('builtin-csv')).queryByTestId('verify-plugin-btn')).not.toBeInTheDocument();
    expect(within(rowById('builtin-csv')).getByTestId('reload-btn')).toBeInTheDocument();
  });

  it('P2-03: the verify button calls reload_plugin and flips the discovered row to ready', async () => {
    const spy = vi.spyOn(ipc, 'reload_plugin');
    await installFixture();

    fireEvent.click(within(rowById('fixture-csv')).getByTestId('verify-plugin-btn'));
    await advance(600);
    expect(spy).toHaveBeenCalledWith({ plugin_id: 'fixture-csv' });
    expect(badgeOf('fixture-csv')).toHaveAttribute('data-state', 'ready');
    expect(badgeOf('fixture-csv')).toHaveTextContent('Ready');
    expect(within(rowById('fixture-csv')).queryByTestId('verify-plugin-btn')).not.toBeInTheDocument();
    expect(within(rowById('fixture-csv')).getByTestId('reload-btn')).toBeInTheDocument();
  });

  it('P2-03: a failed verification surfaces the page error banner', async () => {
    vi.spyOn(ipc, 'reload_plugin').mockRejectedValueOnce({ code: 'internal', message: 'handshake refused' });
    await installFixture();

    fireEvent.click(within(rowById('fixture-csv')).getByTestId('verify-plugin-btn'));
    await advance(600);
    const banner = screen.getByTestId('plugin-page-error');
    expect(banner).toHaveTextContent('验证失败');
    expect(banner).toHaveTextContent('Internal error');
  });

  it('update flow: check finds v1.2.0, confirm updates and the list refreshes to v2.0.0', async () => {
    const checkSpy = vi.spyOn(ipc, 'check_plugin_update');
    const updateSpy = vi.spyOn(ipc, 'update_plugin');
    await installFixture();

    fireEvent.click(within(rowById('fixture-csv')).getByTestId('check-update-btn'));
    await advance(500);
    expect(checkSpy).toHaveBeenCalledWith({ plugin_id: 'fixture-csv' });

    const confirm = screen.getByTestId('update-confirm');
    expect(confirm).toHaveTextContent('Found v1.2.0');
    fireEvent.click(within(confirm).getByTestId('update-confirm-btn'));
    await advance(500);
    expect(updateSpy).toHaveBeenCalledWith({ plugin_id: 'fixture-csv' });
    expect(within(rowById('fixture-csv')).getByText('v2.0.0')).toBeInTheDocument();
  });

  it('details drawer renders 关于 with author/repository/tools', async () => {
    await installFixture();

    fireEvent.click(within(rowById('fixture-csv')).getByTestId('drawer-toggle'));
    await advance(500);
    const drawer = within(rowById('fixture-csv')).getByTestId('plugin-drawer');
    const about = within(drawer).getByTestId('plugin-about');
    expect(about).toHaveTextContent('Author: Fixture Labs');
    expect(about).toHaveTextContent('Repository');
    expect(within(about).getByRole('link')).toHaveAttribute('href', 'https://github.com/fixture/fixture-csv');
    expect(about).toHaveTextContent('Required Tools: AnalysisBuddy >= 0.1.0');
  });

  it('changelog renders semver-desc, marks the current version and dashes empty notes', async () => {
    await installFixture();

    fireEvent.click(within(rowById('fixture-csv')).getByTestId('drawer-toggle'));
    await advance(500);
    const drawer = within(rowById('fixture-csv')).getByTestId('plugin-drawer');
    const log = within(drawer).getByTestId('changelog');

    const entries = log.querySelectorAll('[data-testid="changelog-entry"]');
    expect(entries).toHaveLength(3);
    expect(entries[0]).toHaveTextContent('v1.2.0');
    expect(entries[1]).toHaveTextContent('v1.1.0');
    expect(entries[2]).toHaveTextContent('v1.0.5');

    const current = log.querySelector('[data-testid="changelog-current"]');
    expect(current?.parentElement?.textContent).toContain('v1.1.0');
    expect(within(entries[1] as HTMLElement).getByTestId('changelog-current')).toHaveTextContent('Current');
    expect(within(entries[2] as HTMLElement).getByTestId('changelog-no-notes')).toHaveTextContent('—');
  });

  it('ChangelogSection collapses >20 entries behind 显示全部', () => {
    const entries: ChangelogEntry[] = Array.from({ length: 25 }, (_, i) => ({
      version: `0.${24 - i}.0`,
      date: '2026-08-01',
      notes: [`note ${i}`],
    }));
    const view = render(<ChangelogSection entries={entries} currentVersion="0.0.0" />);
    expect(view.container.querySelectorAll('[data-testid="changelog-entry"]')).toHaveLength(20);
    expect(screen.getByTestId('changelog-show-all')).toBeInTheDocument();

    fireEvent.click(screen.getByTestId('changelog-show-all'));
    expect(view.container.querySelectorAll('[data-testid="changelog-entry"]')).toHaveLength(25);
  });
});

/** Real (production) mode: the ipc singleton must be rebuilt with VITE_AB_IPC=real, so these
 *  tests reset the module registry and dynamically import a fresh component tree (task 7). */
describe('PluginManagerPage real mode: zip dropzone + file picker (task 7)', () => {
  beforeEach(() => {
    vi.stubEnv('VITE_AB_IPC', 'real');
    vi.stubEnv('MODE', 'production');
    tauri.listeners.clear();
    tauri.invoke.mockReset();
    tauri.open.mockReset();
  });

  afterEach(() => {
    vi.unstubAllEnvs();
    vi.resetModules();
  });

  /** install_plugin_zip 必须解析为合法 PluginInfo（否则 reducer 会把 {} upsert 进列表）。 */
  function stubInvoke() {
    tauri.invoke.mockImplementation((cmd: string) => {
      if (cmd === 'install_plugin_zip') {
        return Promise.resolve({
          id: 'picked',
          display_name: 'Picked',
          version: '1.0.0',
          state: 'ready',
          loaded_file_ids: [],
          capabilities: { annotate: false, subscribe: false, binary_sidecar: false },
          last_error: null,
          source: 'portable',
          builtin: false,
          disabled: false,
        });
      }
      if (cmd === 'list_plugins') return Promise.resolve([]);
      return Promise.resolve(null);
    });
  }

  async function renderRealPage() {
    vi.resetModules();
    const [tl, { SessionProvider: RealSessionProvider }, { default: RealPluginManagerPage }] = await Promise.all([
      import('@testing-library/react'),
      import('../state/session'),
      import('./PluginManagerPage'),
    ]);
    const view = tl.render(
      <RealSessionProvider>
        <RealPluginManagerPage />
      </RealSessionProvider>,
    );
    return { tl, view };
  }

  it('subscribes tauri://drag-drop and installs dropped .zip paths', async () => {
    stubInvoke();
    const { tl, view } = await renderRealPage();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_DROP)?.({
          payload: { paths: ['C:\\zips\\my-tool.zip'], position: { x: 0, y: 0 } },
        });
      });
      await tl.waitFor(() =>
        expect(tauri.invoke).toHaveBeenCalledWith('install_plugin_zip', {
          path: 'C:\\zips\\my-tool.zip',
          overwrite: false,
        }),
      );
    } finally {
      view.unmount();
    }
  });

  it('ignores non-zip paths in the drop payload', async () => {
    stubInvoke();
    const { tl, view } = await renderRealPage();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_DROP)).toBe(true));
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_DROP)?.({
          payload: { paths: ['C:\\zips\\readme.txt', 'C:\\zips\\other.zip'], position: { x: 0, y: 0 } },
        });
      });
      await tl.waitFor(() =>
        expect(tauri.invoke).toHaveBeenCalledWith('install_plugin_zip', {
          path: 'C:\\zips\\other.zip',
          overwrite: false,
        }),
      );
      expect(tauri.invoke).not.toHaveBeenCalledWith('install_plugin_zip', {
        path: 'C:\\zips\\readme.txt',
        overwrite: false,
      });
    } finally {
      view.unmount();
    }
  });

  it('drag-enter highlights the dropzone and drag-leave clears it', async () => {
    stubInvoke();
    const { tl, view } = await renderRealPage();
    try {
      await tl.waitFor(() => expect(tauri.listeners.has(EV_OS_DRAG_ENTER)).toBe(true));
      const zone = view.container.querySelector('[data-testid="plugin-dropzone"]') as HTMLElement;
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_ENTER)?.({ payload: {} });
      });
      await tl.waitFor(() => expect(zone.className).toContain('plugin-page__dropzone--over'));
      await tl.act(async () => {
        tauri.listeners.get(EV_OS_DRAG_LEAVE)?.({ payload: {} });
      });
      await tl.waitFor(() => expect(zone.className).not.toContain('plugin-page__dropzone--over'));
    } finally {
      view.unmount();
    }
  });

  it('the choose-file button opens a zip-filtered dialog and installs the picked path', async () => {
    stubInvoke();
    tauri.open.mockResolvedValue(['C:\\zips\\picked.zip']);
    const { tl, view } = await renderRealPage();
    try {
      const btn = view.container.querySelector('[data-testid="pick-plugin-zip-btn"]') as HTMLButtonElement;
      expect(btn).toBeTruthy();
      tl.fireEvent.click(btn);
      await tl.waitFor(() =>
        expect(tauri.open).toHaveBeenCalledWith({
          multiple: false,
          filters: [{ name: 'Plugin ZIP', extensions: ['zip'] }],
          title: 'Install Plugin ZIP',
        }),
      );
      await tl.waitFor(() =>
        expect(tauri.invoke).toHaveBeenCalledWith('install_plugin_zip', {
          path: 'C:\\zips\\picked.zip',
          overwrite: false,
        }),
      );
    } finally {
      view.unmount();
    }
  });

  it('does nothing when the file dialog is cancelled', async () => {
    stubInvoke();
    tauri.open.mockResolvedValue(null);
    const { tl, view } = await renderRealPage();
    try {
      const btn = view.container.querySelector('[data-testid="pick-plugin-zip-btn"]') as HTMLButtonElement;
      tl.fireEvent.click(btn);
      await tl.waitFor(() => expect(tauri.open).toHaveBeenCalled());
      await new Promise((resolve) => setTimeout(resolve, 20));
      expect(tauri.invoke).not.toHaveBeenCalledWith('install_plugin_zip', expect.anything());
    } finally {
      view.unmount();
    }
  });
});
