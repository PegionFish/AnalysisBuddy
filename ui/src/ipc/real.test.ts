/** ui/src/ipc/real.test.ts — real IPC wiring tests with the Tauri bridge mocked:
 *  invoke mapping / error normalization / event payload unwrap (task 13 import entries)
 *  + P10 native-dialog behaviors (localized titles, last-dir defaultPath memory). */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  open: vi.fn(async () => null),
  save: vi.fn(async () => null),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: tauri.listen }));
// Dynamic import inside pickImportFiles; vi.mock covers dynamic imports as well.
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: tauri.open, save: tauri.save }));

import { createRealIpc, EV_OS_DRAG_DROP, pickImportFiles, pickSavePath, pickSessionFile } from './real';
import { open, save } from '@tauri-apps/plugin-dialog';

describe('createRealIpc (Tauri bridge mocked)', () => {
  beforeEach(() => {
    tauri.invoke.mockReset();
    tauri.listen.mockReset();
    tauri.open.mockReset();
    tauri.save.mockReset();
  });

  it('import_files maps to invoke("import_files", args) and passes the result through', async () => {
    const dto = { file_id: 'f1', path: 'a', name: 'a', size_bytes: 1, status: 'ready', matched_plugin: null, candidate_plugins: [] };
    tauri.invoke.mockResolvedValue([dto]);
    const ipc = createRealIpc();

    const results = await ipc.import_files({ paths: ['C:\\a.csv'] });

    expect(tauri.invoke).toHaveBeenCalledWith('import_files', { paths: ['C:\\a.csv'], overrides: undefined });
    expect(results).toEqual([dto]);
  });

  it('normalizes a string rejection into IpcError{code:"internal"}', async () => {
    tauri.invoke.mockRejectedValue('acl denied');
    const ipc = createRealIpc();

    await expect(ipc.import_files({ paths: ['a'] })).rejects.toEqual({
      code: 'internal',
      message: 'acl denied',
      data: 'acl denied',
    });
  });

  it('passes structured IpcError rejections through unchanged', async () => {
    const err = { code: 'invalid_arg', message: 'all paths are empty' };
    tauri.invoke.mockRejectedValue(err);
    const ipc = createRealIpc();

    await expect(ipc.import_files({ paths: [''] })).rejects.toEqual(err);
  });

  it('listen unwraps the event payload and unsubscribes via the resolved unlisten', async () => {
    const handlerSpy = vi.fn();
    const unlisten = vi.fn();
    tauri.listen.mockResolvedValue(unlisten);
    const ipc = createRealIpc();

    const off = ipc.listen<{ paths: string[] }>(EV_OS_DRAG_DROP, handlerSpy);
    expect(tauri.listen).toHaveBeenCalledWith(EV_OS_DRAG_DROP, expect.any(Function));
    const [, handler] = tauri.listen.mock.calls[0];

    handler({ payload: { paths: ['C:\\x.csv'], position: { x: 1, y: 2 } } });
    expect(handlerSpy).toHaveBeenCalledWith({ paths: ['C:\\x.csv'], position: { x: 1, y: 2 } });

    await Promise.resolve(); // let listen() resolve to the unlisten fn
    off();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });

  it('pickImportFiles returns the picked paths and [] when the dialog is cancelled', async () => {
    vi.mocked(open).mockResolvedValueOnce(['C:\\p\\one.csv', 'C:\\p\\two.log']);
    await expect(pickImportFiles()).resolves.toEqual(['C:\\p\\one.csv', 'C:\\p\\two.log']);
    expect(vi.mocked(open)).toHaveBeenCalledWith({ multiple: true });

    vi.mocked(open).mockResolvedValueOnce(null);
    await expect(pickImportFiles()).resolves.toEqual([]);
  });

  it('pickImportFiles remembers the last import dir and defaults to it next time (P10)', async () => {
    // First pick from C:\p → dir remembered in localStorage.
    vi.mocked(open).mockResolvedValueOnce(['C:\\p\\one.csv']);
    await pickImportFiles();
    expect(localStorage.getItem('ab.lastImportDir')).toBe('C:\\p');

    // Next call opens with defaultPath = last dir.
    vi.mocked(open).mockResolvedValueOnce(['C:\\p\\two.log']);
    await pickImportFiles();
    expect(vi.mocked(open)).toHaveBeenLastCalledWith({ multiple: true, defaultPath: 'C:\\p' });
  });

  it('pickImportFiles does not pass defaultPath until a dir is remembered (P10)', async () => {
    vi.mocked(open).mockResolvedValueOnce(null);
    await pickImportFiles();
    expect(vi.mocked(open)).toHaveBeenLastCalledWith({ multiple: true });
  });

  it('pickSavePath localizes the title and cancels to null', async () => {
    vi.mocked(save).mockResolvedValueOnce('C:\\sessions\\s.absession');
    await expect(pickSavePath()).resolves.toEqual('C:\\sessions\\s.absession');
    expect(vi.mocked(save)).toHaveBeenCalledWith({
      filters: [{ name: 'AnalysisBuddy Session', extensions: ['absession'] }],
      defaultPath: 'session.absession',
      title: 'Save AnalysisBuddy Session',
    });

    vi.mocked(save).mockResolvedValueOnce(null);
    await expect(pickSavePath()).resolves.toBeNull();
  });

  it('pickSavePath defaults into the remembered session dir and remembers the new one (P10)', async () => {
    localStorage.setItem('ab.lastSessionDir', 'C:\\sessions');
    vi.mocked(save).mockResolvedValueOnce('C:\\sessions\\other\\s.absession');
    await pickSavePath();
    expect(vi.mocked(save)).toHaveBeenLastCalledWith(
      expect.objectContaining({ defaultPath: 'C:\\sessions\\session.absession' }),
    );
    expect(localStorage.getItem('ab.lastSessionDir')).toBe('C:\\sessions\\other');
  });

  it('pickSessionFile picks a single .absession with a localized title and remembers the dir (P7/P10)', async () => {
    vi.mocked(open).mockResolvedValueOnce('C:\\sessions\\s.absession');
    await expect(pickSessionFile()).resolves.toEqual('C:\\sessions\\s.absession');
    expect(vi.mocked(open)).toHaveBeenLastCalledWith({
      multiple: false,
      filters: [{ name: 'AnalysisBuddy Session', extensions: ['absession'] }],
      title: 'Open AnalysisBuddy Session',
    });
    expect(localStorage.getItem('ab.lastSessionDir')).toBe('C:\\sessions');

    // 取消 → null，目录记忆不变
    localStorage.removeItem('ab.lastSessionDir');
    vi.mocked(open).mockResolvedValueOnce(null);
    await expect(pickSessionFile()).resolves.toBeNull();
    expect(localStorage.getItem('ab.lastSessionDir')).toBeNull();
  });
});
