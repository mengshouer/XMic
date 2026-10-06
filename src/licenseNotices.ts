export const LICENSE_NOTICE_DEFINITIONS = [
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
    path: "../artifacts/webrtc/win-x64/WEBRTC_THIRD_PARTY_NOTICES.txt",
  },
  { id: "mpl", path: "../third-party/licenses/MPL-2.0.txt" },
  {
    id: "mplSources",
    path: "../artifacts/licenses/MPL_SOURCE_CODE.txt",
  },
] as const;

export type LicenseNoticeId = (typeof LICENSE_NOTICE_DEFINITIONS)[number]["id"];

const noticeLoaders = import.meta.glob<string>(
  [
    "../LICENSE",
    "../THIRD_PARTY_NOTICES.md",
    "../artifacts/licenses/CARGO_THIRD_PARTY_NOTICES.txt",
    "../artifacts/licenses/NPM_THIRD_PARTY_NOTICES.txt",
    "../artifacts/webrtc/win-x64/WEBRTC_THIRD_PARTY_NOTICES.txt",
    "../third-party/licenses/MPL-2.0.txt",
    "../artifacts/licenses/MPL_SOURCE_CODE.txt",
  ],
  { query: "?raw", import: "default" },
);

const noticeCache = new Map<LicenseNoticeId, string>();

export async function loadLicenseNotice(id: LicenseNoticeId): Promise<string> {
  const cached = noticeCache.get(id);
  if (cached !== undefined) return cached;

  const definition = LICENSE_NOTICE_DEFINITIONS.find(
    (candidate) => candidate.id === id,
  );
  if (!definition) throw new Error(`Unknown license notice: ${id}`);
  const loader = noticeLoaders[definition.path];
  if (!loader) {
    throw new Error(
      `Embedded license notice is unavailable: ${definition.path}`,
    );
  }
  const content = await loader();
  noticeCache.set(id, content);
  return content;
}
