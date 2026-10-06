import { spawnSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  assertContainedPath,
  assertTreesEqual,
  describeTree,
  fail,
  normalizeCargoLicenseExpression,
  replaceDirectoryFromStaging,
  resolveContainedDirectory,
  resolveContainedFile,
  sha256,
  validateLicenseExpression,
} from "./license-utils.mjs";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const artifactsRoot = join(projectRoot, "artifacts", "licenses");
const cargoLockPath = join(projectRoot, "Cargo.lock");
const npmLockPath = join(projectRoot, "package-lock.json");
const windowsTarget = "x86_64-pc-windows-msvc";
const maxSourceBundleFiles = 1_000;
const maxSourceBundleBytes = 20 * 1024 * 1024;

const allowedLicenseIds = new Set([
  "0BSD",
  "Apache-2.0",
  "BSD-2-Clause",
  "BSD-3-Clause",
  "BlueOak-1.0.0",
  "BSL-1.0",
  "CC0-1.0",
  "ISC",
  "MIT",
  "MIT-0",
  "MPL-2.0",
  "OpenSSL",
  "Unicode-3.0",
  "Unicode-DFS-2016",
  "Unlicense",
  "Zlib",
]);
const allowedLicenseExceptions = new Set(["LLVM-exception"]);
const licenseFilePattern = /^(licen[cs]e|copying|notice|copyright|patents?)([-_.].*)?$/i;
const thirdPartyLicenseRoot = join(
  projectRoot,
  "third-party",
  "licenses",
);
const overrideLicensePaths = {
  allocStdlib: join(thirdPartyLicenseRoot, "alloc-stdlib-BSD-3-Clause.txt"),
  mpl: join(thirdPartyLicenseRoot, "MPL-2.0.txt"),
  tauri: join(thirdPartyLicenseRoot, "tauri-MIT.txt"),
  unicCopyright: join(thirdPartyLicenseRoot, "rust-unic-COPYRIGHT.md"),
  upstreamMit: join(thirdPartyLicenseRoot, "upstream-MIT.txt"),
  webView2: join(thirdPartyLicenseRoot, "webview2-rs-MIT.txt"),
};
const overrideLicenseHashes = new Map([
  [overrideLicensePaths.allocStdlib, "c0c56f26d9c051cac4d200c34c84e7ae9aaa853e01a982a1df08b09931e518ae"],
  [overrideLicensePaths.mpl, "1f256ecad192880510e84ad60474eab7589218784b9a50bc7ceee34c2b91f1d5"],
  [overrideLicensePaths.tauri, "9dd42ea92cff2ede5cd477cbfcce051b2d0115c0ac7f368ee88cb545055dff1d"],
  [overrideLicensePaths.unicCopyright, "f5c342c49f3ac804f3e8e7bb62a8040a44c50d47bb36902b1abd13f66a1adf8b"],
  [overrideLicensePaths.upstreamMit, "23f18e03dc49df91622fe2a76176497404e46ced8a715d9d2b67a7446571cca3"],
  [overrideLicensePaths.webView2, "0dcf41516e608bbcb6cdc5229feb7b86fe4a643b85e7df251133c93408fdac73"],
]);
const cargoLicenseOverrides = new Map([
  [
    "alloc-stdlib@0.2.4",
    {
      paths: [overrideLicensePaths.allocStdlib],
      source:
        "https://github.com/dropbox/rust-alloc-no-stdlib/blob/ae42d22078b98549e987d2f03d12df7b984fde47/LICENSE",
    },
  ],
  [
    "audio-core@0.2.1",
    {
      paths: [overrideLicensePaths.upstreamMit],
      selectedLicense: "MIT",
      source:
        "https://github.com/udoprog/audio/blob/10489fc46bea493755dcd0195b7d7d605e5b72a8/LICENSE-MIT",
    },
  ],
  [
    "realfft@3.5.0",
    {
      paths: [overrideLicensePaths.upstreamMit],
      source:
        "https://github.com/HEnquist/realfft/blob/d0d4eee0525fd27c96c8a046d6d107acd5ed84a6/Cargo.toml",
    },
  ],
  [
    "selectors@0.36.1",
    {
      paths: [overrideLicensePaths.mpl],
      source: "https://www.mozilla.org/MPL/2.0/",
    },
  ],
  [
    "tauri-plugin@2.6.3",
    {
      paths: [overrideLicensePaths.tauri],
      selectedLicense: "MIT",
      source:
        "https://github.com/tauri-apps/tauri/blob/6f6ab1207bb3923c2721fbc67d2fdb1c8deb0c7a/LICENSE_MIT",
    },
  ],
  ...[
    "unic-char-property@0.9.0",
    "unic-char-range@0.9.0",
    "unic-common@0.9.0",
    "unic-ucd-version@0.9.0",
  ].map((packageId) => [
    packageId,
    {
      paths: [overrideLicensePaths.upstreamMit, overrideLicensePaths.unicCopyright],
      selectedLicense: "MIT",
      source:
        "https://github.com/open-i18n/rust-unic/tree/5878605364af97a3358368a6eaef02104af2e016",
    },
  ]),
  [
    "unic-ucd-ident@0.9.0",
    {
      paths: [overrideLicensePaths.upstreamMit, overrideLicensePaths.unicCopyright],
      selectedLicense: "MIT",
      source:
        "https://github.com/open-i18n/rust-unic/tree/8a6ce83063d90b91ae2ce59eddb803edd393fca9",
    },
  ],
  [
    "webview2-com@0.38.2",
    {
      paths: [overrideLicensePaths.webView2],
      source:
        "https://github.com/wravery/webview2-rs/blob/b74dc5e2b394044bea5191052868ce7a106c202c/LICENSE",
    },
  ],
  [
    "webview2-com-sys@0.38.2",
    {
      paths: [overrideLicensePaths.webView2],
      source:
        "https://github.com/wravery/webview2-rs/blob/b74dc5e2b394044bea5191052868ce7a106c202c/LICENSE",
    },
  ],
  [
    "webview2-com-macros@0.8.1",
    {
      paths: [overrideLicensePaths.webView2],
      source:
        "https://github.com/wravery/webview2-rs/blob/dffa41a8a46d3f5565eefbff2de57d38d399f158/LICENSE",
    },
  ],
]);
function normalizedPath(path) {
  return resolve(path).replaceAll("\\", "/").toLowerCase();
}

function findLicenseFiles(packageRoot, declaredLicenseFile) {
  const paths = new Set();
  if (declaredLicenseFile) {
    const declaredPath = resolveContainedFile(
      packageRoot,
      declaredLicenseFile,
      "declared license file",
    );
    paths.add(declaredPath);
  }

  for (const entry of readdirSync(packageRoot, { withFileTypes: true })) {
    if (licenseFilePattern.test(entry.name)) {
      paths.add(
        resolveContainedFile(packageRoot, entry.name, "package license file"),
      );
    }
  }
  return [...paths].sort((left, right) => left.localeCompare(right));
}

function validateOverrideLicense(path) {
  assertContainedPath(thirdPartyLicenseRoot, path, "license override");
  const expectedHash = overrideLicenseHashes.get(path);
  if (!expectedHash) fail(`license override is not hash-pinned: ${path}`);
  const actualHash = sha256(path);
  if (actualHash !== expectedHash) {
    fail(`license override hash mismatch: ${relative(projectRoot, path)}`);
  }
}

function displayLicensePath(packageInfo, licensePath) {
  const packageRelativePath = relative(packageInfo.root, licensePath);
  const outsidePackage =
    isAbsolute(packageRelativePath) ||
    packageRelativePath === ".." ||
    packageRelativePath.startsWith(`..${process.platform === "win32" ? "\\" : "/"}`);
  return relative(outsidePackage ? projectRoot : packageInfo.root, licensePath).replaceAll(
    "\\",
    "/",
  );
}

function renderNotices(title, lockPath, packages) {
  const sections = [
    title,
    "=".repeat(title.length),
    "",
    "This file is generated. Do not edit it by hand.",
    `Lock file: ${relative(projectRoot, lockPath).replaceAll("\\", "/")}`,
    `Lock SHA-256: ${sha256(lockPath)}`,
    `Packages: ${packages.length}`,
    "",
  ];

  for (const packageInfo of packages) {
    sections.push("-".repeat(80));
    sections.push(`Package: ${packageInfo.name} ${packageInfo.version}`);
    sections.push(`License: ${packageInfo.license}`);
    if (packageInfo.selectedLicense) {
      sections.push(`Selected license: ${packageInfo.selectedLicense}`);
    }
    if (packageInfo.declaredLicense) {
      sections.push(`Declared license metadata: ${packageInfo.declaredLicense}`);
      sections.push(`Normalized SPDX expression: ${packageInfo.license}`);
    }
    if (packageInfo.authors?.length) {
      sections.push(`Authors: ${packageInfo.authors.join(", ")}`);
    }
    if (packageInfo.repository) {
      sections.push(`Repository: ${packageInfo.repository}`);
    }
    if (packageInfo.licenseSource) {
      sections.push(`License evidence: ${packageInfo.licenseSource}`);
    }
    if (packageInfo.sourceBundlePath) {
      sections.push(`Source code: ${packageInfo.sourceBundlePath}`);
    }
    sections.push("");
    for (const licensePath of packageInfo.licenseFiles) {
      sections.push(`----- ${displayLicensePath(packageInfo, licensePath)} -----`);
      sections.push(readFileSync(licensePath, "utf8").trimEnd());
      sections.push("");
    }
  }
  return `${sections.join("\n").trimEnd()}\n`;
}

function cargoMetadata() {
  const fetch = spawnSync(
    "cargo",
    ["fetch", "--locked", "--target", windowsTarget],
    { cwd: projectRoot, encoding: "utf8", stdio: "inherit" },
  );
  if (fetch.status !== 0) {
    fail("cargo fetch failed while preparing license metadata");
  }

  const metadata = spawnSync(
    "cargo",
    [
      "metadata",
      "--locked",
      "--filter-platform",
      windowsTarget,
      "--format-version",
      "1",
    ],
    {
      cwd: projectRoot,
      encoding: "utf8",
      maxBuffer: 64 * 1024 * 1024,
    },
  );
  if (metadata.status !== 0) {
    process.stderr.write(metadata.stderr);
    if (metadata.error) {
      process.stderr.write(`${metadata.error.message}\n`);
    }
    fail("cargo metadata failed while preparing license metadata");
  }
  return JSON.parse(metadata.stdout);
}

function cargoPackages() {
  const metadata = cargoMetadata();
  const packagesById = new Map(metadata.packages.map((item) => [item.id, item]));
  const nodesById = new Map(metadata.resolve.nodes.map((item) => [item.id, item]));
  const appManifest = normalizedPath(join(projectRoot, "src-tauri", "Cargo.toml"));
  const appPackage = metadata.packages.find(
    (item) => normalizedPath(item.manifest_path) === appManifest,
  );
  if (!appPackage) {
    fail("cargo metadata did not contain the XMic application package");
  }

  const visited = new Set();
  const queue = [appPackage.id];
  while (queue.length > 0) {
    const packageId = queue.shift();
    if (visited.has(packageId)) continue;
    visited.add(packageId);
    const node = nodesById.get(packageId);
    if (!node) continue;
    for (const dependency of node.deps) {
      const isReleaseDependency = dependency.dep_kinds.some(
        (kind) => kind.kind !== "dev",
      );
      if (isReleaseDependency) queue.push(dependency.pkg);
    }
  }

  return [...visited]
    .map((id) => packagesById.get(id))
    .filter((item) => item?.source)
    .map((item) => {
      const packageName = `${item.name} ${item.version}`;
      const normalizedLicense = normalizeCargoLicenseExpression(item.license);
      const licenseIds = validateLicenseExpression(
        normalizedLicense.expression,
        packageName,
        allowedLicenseIds,
        allowedLicenseExceptions,
      );
      const root = dirname(
        assertContainedPath(
          dirname(item.manifest_path),
          item.manifest_path,
          `${packageName} manifest`,
        ),
      );
      let licenseFiles = findLicenseFiles(root, item.license_file);
      let licenseSource;
      let selectedLicense;
      if (licenseFiles.length === 0) {
        const override = cargoLicenseOverrides.get(`${item.name}@${item.version}`);
        if (override) {
          for (const path of override.paths) {
            validateOverrideLicense(path);
          }
          if (
            override.selectedLicense &&
            !licenseIds.has(override.selectedLicense)
          ) {
            fail(
              `${packageName} override selects ${override.selectedLicense}, ` +
                `which is not present in ${item.license}`,
            );
          }
          licenseFiles = override.paths;
          licenseSource = override.source;
          selectedLicense = override.selectedLicense;
        }
      }
      if (licenseFiles.length === 0) {
        fail(`${packageName} contains no distributable license file`);
      }
      return {
        name: item.name,
        version: item.version,
        license: normalizedLicense.expression,
        declaredLicense: normalizedLicense.declaredLicense,
        licenseIds,
        selectedLicense,
        authors: item.authors,
        ecosystem: "cargo",
        repository: item.repository,
        root,
        licenseFiles,
        licenseSource,
      };
    })
    .sort((left, right) =>
      `${left.name}@${left.version}`.localeCompare(`${right.name}@${right.version}`),
    );
}

function npmPackages() {
  const lock = JSON.parse(readFileSync(npmLockPath, "utf8"));
  const nodeModulesRoot = join(projectRoot, "node_modules");
  if (!existsSync(nodeModulesRoot) || !statSync(nodeModulesRoot).isDirectory()) {
    fail("node_modules is missing; run npm ci before generating notices");
  }
  const packages = [];
  for (const [lockPath, lockInfo] of Object.entries(lock.packages ?? {})) {
    if (!lockPath.startsWith("node_modules/") || lockInfo.dev) continue;
    const packagePath = lockPath.slice("node_modules/".length);
    const candidateRoot = join(nodeModulesRoot, packagePath);
    if (!existsSync(candidateRoot)) {
      if (lockInfo.optional) continue;
      fail(`${lockPath} is missing; run npm ci before generating notices`);
    }
    const root = resolveContainedDirectory(
      nodeModulesRoot,
      packagePath,
      `${lockPath} package root`,
    );
    if (!existsSync(root)) {
      if (lockInfo.optional) continue;
      fail(`${lockPath} is missing; run npm ci before generating notices`);
    }
    const manifestPath = resolveContainedFile(
      root,
      "package.json",
      `${lockPath} manifest`,
    );
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    const packageName = `${manifest.name} ${manifest.version}`;
    if (manifest.version !== lockInfo.version) {
      fail(
        `${packageName} does not match locked version ${lockInfo.version ?? "missing"}`,
      );
    }
    const licenseIds = validateLicenseExpression(
      manifest.license,
      packageName,
      allowedLicenseIds,
      allowedLicenseExceptions,
    );
    const licenseFiles = findLicenseFiles(root, manifest.licenseFile);
    if (licenseFiles.length === 0) {
      fail(`${packageName} contains no distributable license file`);
    }
    const repository =
      typeof manifest.repository === "string"
        ? manifest.repository
        : manifest.repository?.url;
    packages.push({
      name: manifest.name,
      version: manifest.version,
      license: manifest.license,
      licenseIds,
      ecosystem: "npm",
      repository,
      root,
      licenseFiles,
    });
  }
  return packages.sort((left, right) =>
    `${left.name}@${left.version}`.localeCompare(`${right.name}@${right.version}`),
  );
}

function safePathSegment(value) {
  const segment = value.replace(/[^A-Za-z0-9._-]/g, "_");
  if (!segment || segment === "." || segment === "..") {
    fail(`cannot create a source bundle path from ${value}`);
  }
  return segment;
}

function outputPaths(outputRoot) {
  return {
    cargoNoticePath: join(outputRoot, "CARGO_THIRD_PARTY_NOTICES.txt"),
    npmNoticePath: join(outputRoot, "NPM_THIRD_PARTY_NOTICES.txt"),
    sourceBundleRoot: join(outputRoot, "sources"),
    sourceManifestPath: join(outputRoot, "MPL_SOURCE_CODE.txt"),
  };
}

function generateSourceBundles(packages, paths) {
  const sourcePackages = packages.filter((item) => item.licenseIds.has("MPL-2.0"));
  mkdirSync(paths.sourceBundleRoot, { recursive: true });
  validateOverrideLicense(overrideLicensePaths.mpl);
  const destinations = new Set();
  let totalFiles = 0;
  let totalBytes = 0;

  for (const packageInfo of sourcePackages) {
    const directoryName = `${safePathSegment(packageInfo.name)}-${safePathSegment(
      packageInfo.version,
    )}`;
    const relativeDirectory = join(packageInfo.ecosystem, directoryName);
    const portableDirectory = relativeDirectory.replaceAll("\\", "/");
    if (destinations.has(portableDirectory)) {
      fail(`MPL source bundle destination collision: ${portableDirectory}`);
    }
    destinations.add(portableDirectory);

    describeTree(packageInfo.root);
    const destination = join(paths.sourceBundleRoot, relativeDirectory);
    cpSync(packageInfo.root, destination, { recursive: true });

    const bundledLicense = join(destination, "LICENSE-MPL-2.0.txt");
    if (!existsSync(bundledLicense)) {
      cpSync(overrideLicensePaths.mpl, bundledLicense);
    }

    packageInfo.sourceBundlePath = `sources/${portableDirectory}/`;
    const lockFile = packageInfo.ecosystem === "cargo" ? "Cargo.lock" : "package-lock.json";
    const sourceInfo = [
      `Package: ${packageInfo.name} ${packageInfo.version}`,
      `License: ${packageInfo.license}`,
      `Repository: ${packageInfo.repository ?? "not declared"}`,
      `Locked by: ${lockFile}`,
      `Archive: XMic_<version>_MPL-2.0-source.zip`,
      `Archive path: ${packageInfo.sourceBundlePath}`,
      "",
      "The package source files in this directory were copied without modification",
      "from the package installed from the lock file. XMic added this SOURCE_INFO.txt",
      "file and, when absent upstream, LICENSE-MPL-2.0.txt.",
      "",
    ].join("\n");
    writeFileSync(join(destination, "SOURCE_INFO.txt"), sourceInfo);
    packageInfo.sourceTree = describeTree(destination);
    totalFiles += packageInfo.sourceTree.fileCount;
    totalBytes += packageInfo.sourceTree.bytes;
  }

  if (totalFiles > maxSourceBundleFiles || totalBytes > maxSourceBundleBytes) {
    fail(
      `MPL source bundle exceeds the directory policy (${totalFiles} files, ` +
        `${totalBytes} bytes); review whether to switch to a ZIP archive`,
    );
  }

  const lines = [
    "XMic MPL-2.0 Source Code",
    "========================",
    "",
    "This file is generated. Do not edit it by hand.",
    `Cargo Lock SHA-256: ${sha256(cargoLockPath)}`,
    `npm Lock SHA-256: ${sha256(npmLockPath)}`,
    `Packages: ${sourcePackages.length}`,
    `Files: ${totalFiles}`,
    `Bytes: ${totalBytes}`,
    "",
    "This manifest is embedded in XMic.exe for reference. The matching Source Code",
    "Form is distributed separately in XMic_<version>_MPL-2.0-source.zip, which also",
    "carries LICENSE-MPL-2.0.txt at its root.",
    "Paths below are relative to the root of that versioned archive.",
    "",
  ];
  for (const packageInfo of sourcePackages) {
    lines.push("-".repeat(80));
    lines.push(`Package: ${packageInfo.name} ${packageInfo.version}`);
    lines.push(`License: ${packageInfo.license}`);
    if (packageInfo.repository) lines.push(`Repository: ${packageInfo.repository}`);
    lines.push(`Bundled path: ${packageInfo.sourceBundlePath}`);
    lines.push(`Source files: ${packageInfo.sourceTree.fileCount}`);
    lines.push(`Source bytes: ${packageInfo.sourceTree.bytes}`);
    lines.push(`Source tree SHA-256: ${packageInfo.sourceTree.sha256}`);
    lines.push("");
  }
  writeFileSync(paths.sourceManifestPath, `${lines.join("\n").trimEnd()}\n`);
}

function checkGeneratedNotices() {
  if (!existsSync(artifactsRoot)) {
    fail("generated licenses are missing; run npm run licenses:generate");
  }
  const temporaryRoot = mkdtempSync(join(tmpdir(), "xmic-license-check-"));
  const expectedRoot = join(temporaryRoot, "licenses");
  try {
    writeExpectedArtifacts(expectedRoot);
    assertTreesEqual(expectedRoot, artifactsRoot);
  } finally {
    rmSync(temporaryRoot, { force: true, recursive: true });
  }
  console.log("Generated license artifacts exactly match the locked dependencies.");
}

function writeExpectedArtifacts(outputRoot) {
  mkdirSync(outputRoot, { recursive: true });
  const paths = outputPaths(outputRoot);
  const cargoPackageList = cargoPackages();
  const npmPackageList = npmPackages();
  generateSourceBundles([...cargoPackageList, ...npmPackageList], paths);
  writeFileSync(
    paths.cargoNoticePath,
    renderNotices("XMic Cargo Third-Party Notices", cargoLockPath, cargoPackageList),
  );
  writeFileSync(
    paths.npmNoticePath,
    renderNotices("XMic npm Runtime Third-Party Notices", npmLockPath, npmPackageList),
  );
}

function generateNotices() {
  const artifactParent = dirname(artifactsRoot);
  mkdirSync(artifactParent, { recursive: true });
  const stagingRoot = mkdtempSync(join(artifactParent, ".licenses-staging-"));
  try {
    writeExpectedArtifacts(stagingRoot);
    replaceDirectoryFromStaging(stagingRoot, artifactsRoot);
  } finally {
    if (existsSync(stagingRoot)) {
      rmSync(stagingRoot, { force: true, recursive: true });
    }
  }
  const paths = outputPaths(artifactsRoot);
  console.log(`Generated ${relative(projectRoot, paths.cargoNoticePath)}`);
  console.log(`Generated ${relative(projectRoot, paths.npmNoticePath)}`);
  console.log(`Generated ${relative(projectRoot, paths.sourceManifestPath)}`);
}

try {
  if (process.argv.includes("--check")) {
    checkGeneratedNotices();
  } else {
    generateNotices();
  }
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}
