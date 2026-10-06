import { describe, expect, it } from "vitest";
import {
  acceptDraftSettings,
  createSettingsDraft,
  editSettingsDraft,
} from "./settingsDraft";

const applied = { capture: "microphone", gain: 3 };

describe("settings draft", () => {
  it("initializes from a status snapshot and follows changes while clean", () => {
    const initial = createSettingsDraft<typeof applied>();
    expect(acceptDraftSettings(initial, applied)).toEqual({
      settings: applied,
      dirty: false,
    });
    const next = { ...applied, gain: 5 };
    expect(acceptDraftSettings(createSettingsDraft(applied), next)).toEqual({
      settings: next,
      dirty: false,
    });
  });

  it("keeps edits when a request started before them finishes", async () => {
    let draft = createSettingsDraft(applied);
    let resolve!: (settings: typeof applied) => void;
    const response = new Promise<typeof applied>((done) => { resolve = done; });
    const refresh = response.then((settings) => {
      draft = acceptDraftSettings(draft, settings);
    });
    draft = editSettingsDraft(draft, (settings) => ({ ...settings, gain: 6 }));
    draft = editSettingsDraft(draft, (settings) => ({ ...settings, capture: "headset" }));
    const edited = draft;

    resolve(applied);
    await refresh;

    expect(draft).toBe(edited);
    expect(draft.settings).toEqual({ capture: "headset", gain: 6 });
    expect(draft.dirty).toBe(true);
  });

  it("allows an explicit replacement without implicitly acknowledging edits", () => {
    const edited = editSettingsDraft(createSettingsDraft(applied), (settings) => ({ ...settings, gain: 6 }));
    expect(acceptDraftSettings(edited, applied, true)).toEqual({
      settings: applied,
      dirty: true,
    });
  });

  it("resets draft and dirty together on Discard or successful Apply", () => {
    const edited = editSettingsDraft(createSettingsDraft(applied), (settings) => ({ ...settings, gain: 6 }));
    expect(createSettingsDraft(applied)).toEqual({ settings: applied, dirty: false });
    const saved = createSettingsDraft(edited.settings);
    expect(saved).toEqual({ settings: { ...applied, gain: 6 }, dirty: false });
    expect(acceptDraftSettings(saved, applied).settings).toBe(applied);
  });

  it("preserves unsaved edits across status updates after a failed Apply", () => {
    const edited = editSettingsDraft(createSettingsDraft(applied), (settings) => ({ ...settings, gain: 6 }));
    expect(acceptDraftSettings(edited, applied)).toBe(edited);
    expect(edited.dirty).toBe(true);
  });
});
