export interface SettingsDraft<T> {
  settings: T | null;
  dirty: boolean;
}

export function createSettingsDraft<T>(settings: T | null = null): SettingsDraft<T> {
  return { settings, dirty: false };
}

export function editSettingsDraft<T>(
  current: SettingsDraft<T>,
  update: (settings: T) => T,
): SettingsDraft<T> {
  return {
    settings: current.settings === null ? null : update(current.settings),
    dirty: true,
  };
}

export function acceptDraftSettings<T>(
  current: SettingsDraft<T>,
  settings: T,
  force = false,
): SettingsDraft<T> {
  return current.dirty && !force ? current : { ...current, settings };
}
