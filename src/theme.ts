export type ThemePreference = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

export const THEME_STORAGE_KEY = "xmic.theme";
export const DEFAULT_THEME_PREFERENCE: ThemePreference = "system";

export interface ThemeStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function defaultStorage(): ThemeStorage | undefined {
  if (typeof window === "undefined") return undefined;
  return window.localStorage;
}

export function readStoredThemePreference(
  storage: ThemeStorage | undefined = defaultStorage(),
): ThemePreference {
  try {
    const value = storage?.getItem(THEME_STORAGE_KEY);
    return value === "light" || value === "dark" || value === "system"
      ? value
      : DEFAULT_THEME_PREFERENCE;
  } catch {
    return DEFAULT_THEME_PREFERENCE;
  }
}

export function storeThemePreference(
  preference: ThemePreference,
  storage: ThemeStorage | undefined = defaultStorage(),
): void {
  try {
    storage?.setItem(THEME_STORAGE_KEY, preference);
  } catch {
    // A denied storage write should not prevent an in-memory theme change.
  }
}

export function systemPrefersDarkScheme(): boolean {
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

export function resolveTheme(
  preference: ThemePreference,
  systemPrefersDark: boolean,
): ResolvedTheme {
  if (preference === "light" || preference === "dark") {
    return preference;
  }
  return systemPrefersDark ? "dark" : "light";
}

export function applyThemeToDocument(
  preference: ThemePreference,
  systemPrefersDark = systemPrefersDarkScheme(),
  root: HTMLElement = document.documentElement,
): ResolvedTheme {
  const resolved = resolveTheme(preference, systemPrefersDark);
  root.dataset.resolvedTheme = resolved;
  return resolved;
}
