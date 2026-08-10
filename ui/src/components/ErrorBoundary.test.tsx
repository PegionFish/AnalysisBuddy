/** ui/src/components/ErrorBoundary.test.tsx — 任务 17 正式功能：渲染错误屏显。
 *  验证子树渲染期抛错时不再整树静默卸载，而是展示错误原文 + i18n 包装 + 堆栈摘要。 */

import { render, screen } from '@testing-library/react';
import type React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import ErrorBoundary from './ErrorBoundary';

function Bomb(): React.JSX.Element {
  throw new Error('boom: ECharts setOption failed');
}

describe('ErrorBoundary (task 17)', () => {
  beforeEach(() => {
    // React 会把捕获到的渲染错误再打印一次到 console.error；测试中静默。
    vi.spyOn(console, 'error').mockImplementation(() => undefined);
  });

  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it('renders children when there is no error', () => {
    render(
      <ErrorBoundary>
        <div data-testid="child">ok</div>
      </ErrorBoundary>,
    );
    expect(screen.getByTestId('child')).toBeInTheDocument();
  });

  it('shows the error screen with original message instead of unmounting silently', () => {
    render(
      <ErrorBoundary>
        <Bomb />
      </ErrorBoundary>,
    );
    const message = screen.getByTestId('error-screen-message');
    expect(message).toBeInTheDocument();
    expect(message.textContent).toContain('boom: ECharts setOption failed');
    expect(screen.getByTestId('error-screen')).toBeInTheDocument();

    // 持久化取证：release 无 DevTools 时仍可事后读取。
    const raw = localStorage.getItem('ab.last-render-error');
    expect(raw).toBeTruthy();
    expect(JSON.parse(raw!).message).toContain('boom');
  });
});
