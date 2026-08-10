/** ui/src/components/ErrorBoundary.tsx — 顶层渲染错误防线（任务 17 必做正式功能）。
 *  React 渲染/提交期未捕获错误原本会导致整棵 createRoot 树静默卸载（生产全黑、
 *  release 无 DevTools 无任何可见信息）。本边界显示错误文本与堆栈摘要
 *  （i18n 词条包装 + 原文透传），并提供重新加载入口。 */

import React from 'react';
import i18n from '../i18n';
import './ErrorBoundary.css';

interface ErrorBoundaryProps {
  children: React.ReactNode;
}

interface ErrorBoundaryState {
  error: Error | null;
}

export default class ErrorBoundary extends React.Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { error: null };

  static getDerivedStateFromError(error: Error): ErrorBoundaryState {
    return { error };
  }

  componentDidCatch(error: Error, info: React.ErrorInfo): void {
    // 持久化一份到 localStorage，release 包无 DevTools 时仍可事后取证。
    try {
      const record = {
        message: String(error?.message ?? error),
        stack: (error?.stack ?? '').slice(0, 2000),
        componentStack: (info.componentStack ?? '').slice(0, 2000),
        ts_ms: Date.now(),
      };
      localStorage.setItem('ab.last-render-error', JSON.stringify(record));
    } catch {
      /* localStorage 不可用时仅控制台 */
    }
    console.error('[ErrorBoundary] render error', error, info.componentStack);
  }

  render() {
    const { error } = this.state;
    if (!error) return this.props.children;
    const t = i18n.t.bind(i18n);
    const message = String(error.message || error.name || error);
    const stack = error.stack ?? '';
    // 堆栈摘要：取前 5 行，避免整屏刷屏。
    const stackSummary = stack
      .split('\n')
      .slice(0, 5)
      .join('\n');
    return (
      <div className="error-screen" role="alert" data-testid="error-screen">
        <h1 className="error-screen__title">{t('errors.render.title')}</h1>
        <p className="error-screen__hint">{t('errors.render.hint')}</p>
        <pre className="error-screen__message" data-testid="error-screen-message">
          {message}
        </pre>
        {stackSummary && <pre className="error-screen__stack">{stackSummary}</pre>}
        <button type="button" className="error-screen__reload" onClick={() => window.location.reload()}>
          {t('errors.render.reload')}
        </button>
      </div>
    );
  }
}
