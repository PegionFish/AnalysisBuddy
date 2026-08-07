/** ui/src/i18n/index.ts — i18next bootstrap (ipc-ui.md §6).
 *  Language persisted in localStorage key `ab.lang`; default follows navigator.language (zh→zh, else en). */

import i18n from 'i18next';
import { initReactI18next } from 'react-i18next';
import zh from './zh.json';
import en from './en.json';

export const STORAGE_KEY_LANG = 'ab.lang';

const stored = localStorage.getItem(STORAGE_KEY_LANG);
const initialLang = stored === 'zh' || stored === 'en' ? stored : navigator.language.toLowerCase().startsWith('zh') ? 'zh' : 'en';

void i18n.use(initReactI18next).init({
  resources: {
    zh: { translation: zh },
    en: { translation: en },
  },
  lng: initialLang,
  fallbackLng: 'en',
  interpolation: { escapeValue: false },
});

i18n.on('languageChanged', (lng) => {
  localStorage.setItem(STORAGE_KEY_LANG, lng);
});

export default i18n;
