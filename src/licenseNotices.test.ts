import { describe, expect, it } from "vitest";
import { LICENSE_NOTICE_DEFINITIONS } from "./licenseNotices";

describe("embedded license notice manifest", () => {
  it("keeps the approved seven lazy notice inputs in one fixed manifest", () => {
    expect(LICENSE_NOTICE_DEFINITIONS).toEqual([
      { id: "xmic", path: "../LICENSE" },
      { id: "thirdParty", path: "../THIRD_PARTY_NOTICES.md" },
      {
        id: "cargo",
        path: "../artifacts/licenses/CARGO_THIRD_PARTY_NOTICES.txt",
      },
      {
        id: "npm",
        path: "../artifacts/licenses/NPM_THIRD_PARTY_NOTICES.txt",
      },
      {
        id: "webrtc",
        path:
          "../artifacts/webrtc/win-x64/WEBRTC_THIRD_PARTY_NOTICES.txt",
      },
      { id: "mpl", path: "../third-party/licenses/MPL-2.0.txt" },
      {
        id: "mplSources",
        path: "../artifacts/licenses/MPL_SOURCE_CODE.txt",
      },
    ]);
  });

  it("does not use absolute or traversal paths", () => {
    for (const notice of LICENSE_NOTICE_DEFINITIONS) {
      expect(notice.path).toMatch(/^\.\.\/(?!\.\.\/)/);
      expect(notice.path).not.toContain("\\");
    }
  });
});
