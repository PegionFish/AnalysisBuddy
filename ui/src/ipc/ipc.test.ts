import { describe, expect, it, vi } from 'vitest';
import { useMockIpc } from './ipc';

describe('useMockIpc environment switch (ipc-ui.md §3.2)', () => {
  it('forces mock when VITE_AB_IPC=mock', () => {
    vi.stubEnv('VITE_AB_IPC', 'mock');
    vi.stubEnv('MODE', 'production');
    expect(useMockIpc()).toBe(true);
    vi.unstubAllEnvs();
  });

  it('forces real when VITE_AB_IPC=real outside development', () => {
    vi.stubEnv('VITE_AB_IPC', 'real');
    vi.stubEnv('MODE', 'production');
    expect(useMockIpc()).toBe(false);
    vi.unstubAllEnvs();
  });

  it('explicit VITE_AB_IPC=real wins over development MODE（F3：IPC 契约回归开发期可见）', () => {
    vi.stubEnv('VITE_AB_IPC', 'real');
    vi.stubEnv('MODE', 'development');
    expect(useMockIpc()).toBe(false);
    vi.unstubAllEnvs();
  });
});
