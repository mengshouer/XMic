import { describe, expect, it } from "vitest";
import {
  applyThemeToDocument,
  DEFAULT_THEME_PREFERENCE,
  readStoredThemePreference,
  resolveTheme,
  storeThemePreference,
  THEME_STORAGE_KEY,
  type ThemeStorage,
} from "./theme";

function createMemoryStorage(initial: Record<string, string> = {}): ThemeStorage & {
  data: Record<string, string>;
} {
  const data = { ...initial };
  return {
    data,
    getItem(key: string) {
      return key in data ? data[key] : null;
    },
    setItem(key: string, value: string) {
      data[key] = value;
    },
  };
}

describe("resolveTheme", () => {
  it("follows system dark/light preference when set to system", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
  });

  it("honors explicit light or dark overrides regardless of system preference", () => {
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("light", false)).toBe("light");
    expect(resolveTheme("dark", true)).toBe("dark");
    expect(resolveTheme("dark", false)).toBe("dark");
  });
});

describe("theme storage and document application", () => {
  it("reads valid stored preferences and falls back to system for missing or invalid values", () => {
    expect(readStoredThemePreference(createMemoryStorage())).toBe(
      DEFAULT_THEME_PREFERENCE,
    );
    expect(
      readStoredThemePreference(
        createMemoryStorage({ [THEME_STORAGE_KEY]: "light" }),
      ),
    ).toBe("light");
    expect(
      readStoredThemePreference(
        createMemoryStorage({ [THEME_STORAGE_KEY]: "dark" }),
      ),
    ).toBe("dark");
    expect(
      readStoredThemePreference(
        createMemoryStorage({ [THEME_STORAGE_KEY]: "system" }),
      ),
    ).toBe("system");
    expect(
      readStoredThemePreference(
        createMemoryStorage({ [THEME_STORAGE_KEY]: "sepia" }),
      ),
    ).toBe(DEFAULT_THEME_PREFERENCE);
  });

  it("tolerates unavailable or throwing storage implementations", () => {
    const throwingStorage: ThemeStorage = {
      getItem() {
        throw new Error("storage blocked");
      },
      setItem() {
        throw new Error("storage blocked");
      },
    };
    expect(readStoredThemePreference(undefined)).toBe(DEFAULT_THEME_PREFERENCE);
    expect(readStoredThemePreference(throwingStorage)).toBe(
      DEFAULT_THEME_PREFERENCE,
    );
    expect(() => storeThemePreference("dark", throwingStorage)).not.toThrow();
  });

  it("persists theme preference and writes resolved dataset attributes", () => {
    const storage = createMemoryStorage();
    storeThemePreference("light", storage);
    expect(storage.data[THEME_STORAGE_KEY]).toBe("light");

    const fakeRoot = { dataset: {} as DOMStringMap } as HTMLElement;
    const resolved = applyThemeToDocument("system", false, fakeRoot);
    expect(resolved).toBe("light");
    expect(fakeRoot.dataset.resolvedTheme).toBe("light");
  });
});
