/** ui/src/ipc/real.test.ts — real IPC wiring tests with the Tauri bridge mocked:
 *  invoke mapping / error normalization / event payload unwrap (task 13 import entries). */
import { beforeEach, describe, expect, it, vi } from 'vitest';

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: tauri.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: tauri.listen }));
// Dynamic import inside pickImportFiles; vi.mock covers dynamic imports as well.
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(async () => null) }));

import { createRealIpc, EV_OS_DRAG_DROP, pickImportFiles } from './real';
import { open } from '@tauri-apps/plugin-dialog';

describe('createRealIpc (Tauri bridge mocked)', () => {
  beforeEach(() => {
    tauri.invoke.mockReset();
    tauri.listen.mockReset();
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
});
