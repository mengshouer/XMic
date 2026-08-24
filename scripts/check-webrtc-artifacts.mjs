import {
  lstatSync,
  readFileSync,
  readdirSync,
  statSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  fail,
  resolveContainedFile,
  sha256,
} from "./license-utils.mjs";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const defaultArtifactRoot = join(projectRoot, "artifacts", "webrtc", "win-x64");
const defaultLockPath = join(
  projectRoot,
  "native",
  "webrtc-audio-processing",
  "webrtc-lock.json",
);
const expectedArtifactNames = new Set([
  "PROVENANCE.json",
  "WEBRTC_THIRD_PARTY_NOTICES.txt",
  "xmic_webrtc_dsp.dll",
]);
const expectedOverlayPaths = new Set([
  "native/webrtc-audio-processing/BUILD.gn",
  "native/webrtc-audio-processing/include/xmic_webrtc_dsp.h",
  "native/webrtc-audio-processing/src/xmic_webrtc_dsp.cc",
]);
const sha256Pattern = /^[a-f0-9]{64}$/;
const gitHashPattern = /^[a-f0-9]{40}$/;

function resolvePathOption(name, fallback) {
  const index = process.argv.indexOf(name);
  if (index === -1) return fallback;
  const value = process.argv[index + 1];
  if (!value || value.startsWith("--")) fail(`${name} requires a path`);
  return resolve(value);
}

function requireString(value, description, pattern) {
  if (typeof value !== "string" || value.length === 0) {
    fail(`${description} must be a non-empty string`);
  }
  if (pattern && !pattern.test(value)) fail(`${description} has an invalid format`);
  return value;
}

function requireEqual(actual, expected, description) {
  if (actual !== expected) {
    fail(`${description} mismatch: expected ${expected}, got ${actual}`);
  }
}

function requireStringArray(value, description) {
  if (
    !Array.isArray(value) ||
    value.length === 0 ||
    value.some((item) => typeof item !== "string" || item.length === 0)
  ) {
    fail(`${description} must be a non-empty string array`);
  }
  if (new Set(value).size !== value.length) {
    fail(`${description} contains duplicate entries`);
  }
  return value;
}

function readJson(path, description) {
  let parsed;
  try {
    parsed = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    fail(`${description} is not valid JSON: ${detail}`);
  }
  return parsed;
}

function checkArtifactDirectory(artifactRoot) {
  const names = readdirSync(artifactRoot).sort();
  for (const name of names) {
    const path = join(artifactRoot, name);
    if (lstatSync(path).isSymbolicLink()) {
      fail(`WebRTC artifact must not be a symbolic link: ${name}`);
    }
    if (!expectedArtifactNames.has(name)) {
      fail(`unexpected WebRTC artifact: ${name}`);
    }
  }
  for (const name of expectedArtifactNames) {
    if (!names.includes(name)) fail(`missing WebRTC artifact: ${name}`);
  }
}

function checkOverlayInputs(inputs) {
  if (!Array.isArray(inputs) || inputs.length !== expectedOverlayPaths.size) {
    fail("WebRTC provenance has an incomplete declaredOverlayInputs list");
  }
  const seen = new Set();
  for (const input of inputs) {
    const path = requireString(input?.path, "overlay input path");
    if (!expectedOverlayPaths.has(path) || seen.has(path)) {
      fail(`unexpected or duplicate WebRTC overlay input: ${path}`);
    }
    seen.add(path);
    const inputPath = resolveContainedFile(projectRoot, path, "overlay input");
    requireEqual(
      sha256(inputPath),
      requireString(input.sha256, `${path} SHA-256`, sha256Pattern),
      `${path} SHA-256`,
    );
  }
}

function check() {
  const artifactRoot = resolvePathOption("--artifact-root", defaultArtifactRoot);
  const lockPath = resolvePathOption("--lock", defaultLockPath);
  if (!statSync(artifactRoot).isDirectory()) {
    fail(`WebRTC artifact root is not a directory: ${artifactRoot}`);
  }
  checkArtifactDirectory(artifactRoot);

  const lock = readJson(lockPath, "WebRTC lock");
  requireEqual(lock.schemaVersion, 1, "WebRTC lock schemaVersion");
  requireString(lock.sourceUrl, "WebRTC source URL");
  requireString(lock.sourceRevision, "WebRTC source revision", gitHashPattern);
  requireString(lock.depsBlob, "WebRTC DEPS blob", gitHashPattern);
  requireString(lock.depotToolsUrl, "depot_tools URL");
  requireString(lock.depotToolsRevision, "depot_tools revision", gitHashPattern);
  requireString(lock.gnTarget, "WebRTC GN target");
  const gnArgs = requireStringArray(lock.gnArgs, "WebRTC GN args");
  requireString(lock.windowsSdkVersion, "Windows SDK version");
  requireString(lock.dllSha256, "WebRTC DLL SHA-256", sha256Pattern);
  requireString(lock.noticesSha256, "WebRTC notices SHA-256", sha256Pattern);
  requireString(lock.provenanceSha256, "WebRTC provenance SHA-256", sha256Pattern);

  const dllPath = resolveContainedFile(
    artifactRoot,
    "xmic_webrtc_dsp.dll",
    "WebRTC DLL",
  );
  const noticePath = resolveContainedFile(
    artifactRoot,
    "WEBRTC_THIRD_PARTY_NOTICES.txt",
    "WebRTC notices",
  );
  const provenancePath = resolveContainedFile(
    artifactRoot,
    "PROVENANCE.json",
    "WebRTC provenance",
  );
  requireEqual(sha256(dllPath), lock.dllSha256, "WebRTC DLL SHA-256");
  requireEqual(sha256(noticePath), lock.noticesSha256, "WebRTC notices SHA-256");
  requireEqual(
    sha256(provenancePath),
    lock.provenanceSha256,
    "WebRTC provenance SHA-256",
  );

  const notice = readFileSync(noticePath, "utf8");
  for (const marker of [
    `WebRTC revision: ${lock.sourceRevision}`,
    `WebRTC DEPS blob: ${lock.depsBlob}`,
    `GN target: ${lock.gnTarget}`,
    `Binary SHA-256: ${lock.dllSha256}`,
  ]) {
    if (!notice.includes(marker)) fail(`WebRTC notices are missing: ${marker}`);
  }
  const componentCount = Number(
    notice.match(/^Third-party components: (\d+)$/m)?.[1] ?? 0,
  );
  if (componentCount <= 0) {
    fail("WebRTC notices contain no third-party GN components");
  }

  const provenance = readJson(provenancePath, "WebRTC provenance");
  requireEqual(provenance.schemaVersion, 1, "WebRTC provenance schemaVersion");
  requireEqual(provenance.subject?.name, "xmic_webrtc_dsp.dll", "provenance subject");
  requireEqual(
    provenance.subject?.sha256,
    lock.dllSha256,
    "provenance subject SHA-256",
  );
  requireEqual(
    provenance.notices?.name,
    "WEBRTC_THIRD_PARTY_NOTICES.txt",
    "provenance notices name",
  );
  requireEqual(
    provenance.notices?.sha256,
    lock.noticesSha256,
    "provenance notices SHA-256",
  );
  requireEqual(
    provenance.materials?.webrtc?.repository,
    lock.sourceUrl,
    "provenance WebRTC repository",
  );
  requireEqual(
    provenance.materials?.webrtc?.revision,
    lock.sourceRevision,
    "provenance WebRTC revision",
  );
  requireEqual(
    provenance.materials?.webrtc?.depsBlob,
    lock.depsBlob,
    "provenance WebRTC DEPS blob",
  );
  requireEqual(
    provenance.materials?.depotTools?.repository,
    lock.depotToolsUrl,
    "provenance depot_tools repository",
  );
  requireEqual(
    provenance.materials?.depotTools?.revision,
    lock.depotToolsRevision,
    "provenance depot_tools revision",
  );
  checkOverlayInputs(provenance.materials?.declaredOverlayInputs);
  requireEqual(provenance.build?.platform, "windows-x64", "provenance platform");
  requireEqual(provenance.build?.target, lock.gnTarget, "provenance GN target");
  requireEqual(
    JSON.stringify(provenance.build?.gnArgs),
    JSON.stringify(gnArgs),
    "provenance GN args",
  );
  requireEqual(
    provenance.build?.windowsSdkVersion,
    lock.windowsSdkVersion,
    "provenance Windows SDK version",
  );
  requireEqual(provenance.build?.capture, "generated", "provenance capture");
  requireString(provenance.build?.builder?.visualStudioVersion, "Visual Studio version");
  requireString(provenance.build?.builder?.gnVersion, "GN version");
  requireString(provenance.build?.builder?.ninjaVersion, "Ninja version");
  console.log("WebRTC DLL, notices, provenance, and lock hashes are consistent.");
}

try {
  check();
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}
