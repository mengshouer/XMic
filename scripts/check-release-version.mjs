import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function fail(message) {
  throw new Error(message);
}

function readJson(path) {
  return JSON.parse(readFileSync(join(projectRoot, path), "utf8"));
}

function workspaceVersion() {
  const manifest = readFileSync(join(projectRoot, "Cargo.toml"), "utf8");
  const heading = "[workspace.package]";
  const sectionStart = manifest.indexOf(heading);
  if (sectionStart === -1) fail("Cargo.toml has no workspace.package section");
  const sectionBody = manifest.slice(sectionStart + heading.length);
  const nextSection = sectionBody.search(/^\[/m);
  const section = nextSection === -1 ? sectionBody : sectionBody.slice(0, nextSection);
  const version = section?.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version) fail("Cargo.toml has no workspace package version");
  return version;
}

try {
  const cargoVersion = workspaceVersion();
  const packageManifest = readJson("package.json");
  const packageLock = readJson("package-lock.json");
  const tauriConfig = readJson("src-tauri/tauri.conf.json");
  const versions = new Map([
    ["Cargo.toml", cargoVersion],
    ["package.json", packageManifest.version],
    ["package-lock.json", packageLock.version],
    ["package-lock.json packages['']", packageLock.packages?.[""]?.version],
    ["src-tauri/tauri.conf.json", tauriConfig.version],
  ]);
  for (const [source, version] of versions) {
    if (version !== cargoVersion) {
      fail(`version mismatch: Cargo.toml=${cargoVersion}, ${source}=${version}`);
    }
  }
  if (!/^\d+\.\d+\.\d+$/.test(cargoVersion)) {
    fail(`release version must be MAJOR.MINOR.PATCH: ${cargoVersion}`);
  }

  const releaseTag = process.env.RELEASE_TAG;
  if (releaseTag !== undefined && releaseTag !== `v${cargoVersion}`) {
    fail(`release tag ${releaseTag} does not match v${cargoVersion}`);
  }
  console.log(`Release version is consistent: ${cargoVersion}`);
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}
