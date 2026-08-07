import { useTranslation } from 'react-i18next';

/** Temporary bootstrap placeholder until AppShell lands (C-02). */
export default function Bootstrap() {
  const { t } = useTranslation();
  return <div>{t('common.app.name')}</div>;
}
