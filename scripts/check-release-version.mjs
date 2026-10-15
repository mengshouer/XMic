import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const SEMVER_PATTERN = /^(\d+)\.(\d+)\.(\d+)$/;

function fail(message) {
  throw new Error(message);
}

function detectEol(text) {
  return text.includes("\r\n") ? "\r\n" : "\n";
}

function readJson(root, relativePath) {
  return JSON.parse(readFileSync(join(root, relativePath), "utf8"));
}

function writeJsonPreservingEol(root, relativePath, value) {
  const fullPath = join(root, relativePath);
  const existing = readFileSync(fullPath, "utf8");
  const eol = detectEol(existing);
  const serialized = `${JSON.stringify(value, null, 2)}\n`.replace(/\n/g, eol);
  writeFileSync(fullPath, serialized, "utf8");
}

function extractTomlSection(manifest, heading, fileLabel) {
  const escapedHeading = heading.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const headingMatch = new RegExp(`^${escapedHeading}\\s*$`, "m").exec(
    manifest,
  );
  if (!headingMatch) fail(`${fileLabel} has no ${heading} section`);
  const bodyStart = headingMatch.index + headingMatch[0].length;
  const rest = manifest.slice(bodyStart);
  const nextSectionOffset = rest.search(/^\[+[A-Za-z0-9_.-]+\]+/m);
  const bodyEnd =
    nextSectionOffset === -1 ? manifest.length : bodyStart + nextSectionOffset;
  return {
    body: manifest.slice(bodyStart, bodyEnd),
    bodyStart,
    bodyEnd,
  };
}

export function readWorkspaceVersion(root = projectRoot) {
  const manifest = readFileSync(join(root, "Cargo.toml"), "utf8");
  const { body } = extractTomlSection(
    manifest,
    "[workspace.package]",
    "Cargo.toml",
  );
  const version = body.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version) fail("Cargo.toml has no workspace package version");
  return version;
}

export function readWorkspaceMemberNames(root = projectRoot) {
  const manifest = readFileSync(join(root, "Cargo.toml"), "utf8");
  const { body } = extractTomlSection(manifest, "[workspace]", "Cargo.toml");
  const membersMatch = body.match(/members\s*=\s*\[([\s\S]*?)\]/m);
  if (!membersMatch) fail("Cargo.toml has no workspace members list");
  const memberPaths = [...membersMatch[1].matchAll(/"([^"]+)"/g)].map(
    (match) => match[1],
  );
  if (memberPaths.length === 0) fail("Cargo.toml workspace members list is empty");

  return memberPaths.map((memberPath) => {
    const memberManifestPath = `${memberPath}/Cargo.toml`;
    const memberManifest = readFileSync(join(root, memberManifestPath), "utf8");
    const { body: pkgBody } = extractTomlSection(
      memberManifest,
      "[package]",
      memberManifestPath,
    );
    const name = pkgBody.match(/^name\s*=\s*"([^"]+)"\s*$/m)?.[1];
    if (!name) fail(`${memberManifestPath} has no package name`);
    return name;
  });
}

function readCargoLockLocalVersions(root, memberNames) {
  const lockText = readFileSync(join(root, "Cargo.lock"), "utf8");
  const blocks = lockText.split(/\r?\n\r?\n/);
  const localVersions = new Map();

  for (const block of blocks) {
    if (!block.startsWith("[[package]]")) continue;
    if (/^source\s*=/m.test(block)) continue;
    const name = block.match(/^name\s*=\s*"([^"]+)"\s*$/m)?.[1];
    const version = block.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
    if (name && version) {
      localVersions.set(name, version);
    }
  }

  const results = [];
  for (const memberName of memberNames) {
    const lockVersion = localVersions.get(memberName);
    if (!lockVersion) {
      fail(`Cargo.lock has no local workspace package entry for ${memberName}`);
    }
    results.push([`Cargo.lock (${memberName})`, lockVersion]);
  }
  return results;
}

export function resolveNextVersion(currentVersion, targetSpec) {
  const currentMatch = currentVersion.match(SEMVER_PATTERN);
  if (!currentMatch) {
    fail(`current version is not MAJOR.MINOR.PATCH: ${currentVersion}`);
  }
  if (!targetSpec || typeof targetSpec !== "string" || !targetSpec.trim()) {
    fail(
      "missing target version. Usage: npm run version:bump -- <MAJOR.MINOR.PATCH|patch|minor|major>",
    );
  }

  const spec = targetSpec.trim();
  const major = Number(currentMatch[1]);
  const minor = Number(currentMatch[2]);
  const patch = Number(currentMatch[3]);

  if (spec === "patch") return `${major}.${minor}.${patch + 1}`;
  if (spec === "minor") return `${major}.${minor + 1}.0`;
  if (spec === "major") return `${major + 1}.0.0`;

  const normalized = spec.startsWith("v") ? spec.slice(1) : spec;
  if (!SEMVER_PATTERN.test(normalized)) {
    fail(
      `invalid target version '${targetSpec}'; expected MAJOR.MINOR.PATCH or patch|minor|major`,
    );
  }
  return normalized;
}

export function verifyReleaseVersion(
  root = projectRoot,
  releaseTag = process.env.RELEASE_TAG,
) {
  const cargoVersion = readWorkspaceVersion(root);
  if (!SEMVER_PATTERN.test(cargoVersion)) {
    fail(`release version must be MAJOR.MINOR.PATCH: ${cargoVersion}`);
  }

  const memberNames = readWorkspaceMemberNames(root);
  const cargoLockEntries = readCargoLockLocalVersions(root, memberNames);
  const packageManifest = readJson(root, "package.json");
  const packageLock = readJson(root, "package-lock.json");
  const tauriConfig = readJson(root, "src-tauri/tauri.conf.json");

  const versions = [
    ["Cargo.toml", cargoVersion],
    ["package.json", packageManifest.version],
    ["package-lock.json", packageLock.version],
    ["package-lock.json packages['']", packageLock.packages?.[""]?.version],
    ["src-tauri/tauri.conf.json", tauriConfig.version],
    ...cargoLockEntries,
  ];

  for (const [source, version] of versions) {
    if (version !== cargoVersion) {
      fail(`version mismatch: Cargo.toml=${cargoVersion}, ${source}=${version}`);
    }
  }

  if (
    releaseTag !== undefined &&
    releaseTag !== null &&
    releaseTag !== "" &&
    releaseTag !== `v${cargoVersion}`
  ) {
    fail(`release tag ${releaseTag} does not match v${cargoVersion}`);
  }

  return cargoVersion;
}

function updateCargoTomlVersion(root, nextVersion) {
  const cargoTomlPath = join(root, "Cargo.toml");
  const manifest = readFileSync(cargoTomlPath, "utf8");
  const { body, bodyStart, bodyEnd } = extractTomlSection(
    manifest,
    "[workspace.package]",
    "Cargo.toml",
  );
  if (!/^version\s*=\s*"[^"]+"\s*$/m.test(body)) {
    fail("Cargo.toml [workspace.package] has no version field to update");
  }
  const updatedBody = body.replace(
    /^version\s*=\s*"[^"]+"\s*$/m,
    `version = "${nextVersion}"`,
  );
  writeFileSync(
    cargoTomlPath,
    `${manifest.slice(0, bodyStart)}${updatedBody}${manifest.slice(bodyEnd)}`,
    "utf8",
  );
}

function updateCargoLockVersions(root, memberNames, nextVersion) {
  const cargoLockPath = join(root, "Cargo.lock");
  const lockText = readFileSync(cargoLockPath, "utf8");
  const memberSet = new Set(memberNames);
  const seen = new Set();

  const updated = lockText.replace(
    /(\[\[package\]\]\r?\nname = "([^"]+)"\r?\nversion = )"([^"]+)"(?=\r?\n(?!source = ))/g,
    (full, prefix, name) => {
      if (!memberSet.has(name)) return full;
      seen.add(name);
      return `${prefix}"${nextVersion}"`;
    },
  );

  for (const memberName of memberNames) {
    if (!seen.has(memberName)) {
      fail(`Cargo.lock is missing local workspace package ${memberName}`);
    }
  }

  writeFileSync(cargoLockPath, updated, "utf8");
}

export function bumpReleaseVersion(targetSpec, root = projectRoot) {
  const previousVersion = readWorkspaceVersion(root);
  const nextVersion = resolveNextVersion(previousVersion, targetSpec);
  const memberNames = readWorkspaceMemberNames(root);

  updateCargoTomlVersion(root, nextVersion);
  updateCargoLockVersions(root, memberNames, nextVersion);

  const packageManifest = readJson(root, "package.json");
  packageManifest.version = nextVersion;
  writeJsonPreservingEol(root, "package.json", packageManifest);

  const packageLock = readJson(root, "package-lock.json");
  packageLock.version = nextVersion;
  if (!packageLock.packages || !packageLock.packages[""]) {
    fail("package-lock.json is missing packages['']");
  }
  packageLock.packages[""].version = nextVersion;
  writeJsonPreservingEol(root, "package-lock.json", packageLock);

  const tauriConfig = readJson(root, "src-tauri/tauri.conf.json");
  tauriConfig.version = nextVersion;
  writeJsonPreservingEol(root, "src-tauri/tauri.conf.json", tauriConfig);

  const verified = verifyReleaseVersion(root, null);
  return {
    previousVersion,
    nextVersion: verified,
  };
}

export function stripLeadingDoubleDash(args) {
  return args[0] === "--" ? args.slice(1) : args;
}

const isMain =
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (isMain) {
  try {
    const args = stripLeadingDoubleDash(process.argv.slice(2));
    if (args.length > 1 || (args.length === 1 && args[0] !== "--print")) {
      fail("Usage: node scripts/check-release-version.mjs [--print]");
    }
    const printOnly = args[0] === "--print";
    const version = verifyReleaseVersion();
    if (printOnly) {
      process.stdout.write(`${version}\n`);
    } else {
      console.log(`Release version is consistent: ${version}`);
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
