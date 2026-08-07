import '@testing-library/jest-dom/vitest';
import { cleanup } from '@testing-library/react';
import { afterEach } from 'vitest';
import { resetAllMockIpc } from '../ipc/mock';

afterEach(() => {
  cleanup();
  localStorage.clear();
  resetAllMockIpc();
});
