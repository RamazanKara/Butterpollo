export type Theme = 'system' | 'light' | 'dark';

const KEY = 'butterpollo.theme';

export function storedTheme(): Theme {
  const value = localStorage.getItem(KEY);
  return value === 'light' || value === 'dark' ? value : 'system';
}
export function applyTheme(theme: Theme) {
  document.documentElement.dataset.theme = theme;
  if (theme === 'system') localStorage.removeItem(KEY);
  else localStorage.setItem(KEY, theme);
}
