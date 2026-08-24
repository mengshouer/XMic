import { createHash, randomUUID } from "node:crypto";
import {
  existsSync,
  lstatSync,
  readFileSync,
  readdirSync,
  realpathSync,
  renameSync,
  rmSync,
  statSync,
} from "node:fs";
import {
  dirname,
  isAbsolute,
  relative,
  resolve,
  sep,
} from "node:path";
import parseSpdxExpression from "spdx-expression-parse";

const cargoLegacyLicenseExpressions = new Map([
  ["MIT/Apache-2.0", "MIT OR Apache-2.0"],
  ["Apache-2.0/MIT", "Apache-2.0 OR MIT"],
  ["BSD-3-Clause/MIT", "BSD-3-Clause OR MIT"],
  ["Unlicense/MIT", "Unlicense OR MIT"],
]);

export function fail(message) {
  throw new Error(message);
}

export function sha256(path) {
  return createHash("sha256").update(readFileSync(path)).digest("hex");
}

function canonicalExistingPath(path, description) {
  if (!existsSync(path)) {
    fail(`${description} does not exist: ${path}`);
  }
  return realpathSync.native(path);
}

function isOutside(root, candidate) {
  const relativePath = relative(root, candidate);
  return (
    isAbsolute(relativePath) ||
    relativePath === ".." ||
    relativePath.startsWith(`..${sep}`)
  );
}

export function assertContainedPath(root, candidate, description) {
  const canonicalRoot = canonicalExistingPath(root, "containment root");
  const canonicalCandidate = canonicalExistingPath(candidate, description);
  if (isOutside(canonicalRoot, canonicalCandidate)) {
    fail(`${description} escapes its allowed root: ${candidate}`);
  }
  return canonicalCandidate;
}

export function resolveContainedFile(root, declaredPath, description) {
  if (typeof declaredPath !== "string" || declaredPath.length === 0) {
    fail(`${description} must be a non-empty relative path`);
  }
  if (isAbsolute(declaredPath)) {
    fail(`${description} must not be absolute: ${declaredPath}`);
  }
  const resolved = assertContainedPath(
    root,
    resolve(root, declaredPath),
    description,
  );
  if (!statSync(resolved).isFile()) {
    fail(`${description} is not a file: ${resolved}`);
  }
  return resolved;
}

export function resolveContainedDirectory(root, declaredPath, description) {
  if (typeof declaredPath !== "string" || declaredPath.length === 0) {
    fail(`${description} must be a non-empty relative path`);
  }
  if (isAbsolute(declaredPath)) {
    fail(`${description} must not be absolute: ${declaredPath}`);
  }
  const resolved = assertContainedPath(
    root,
    resolve(root, declaredPath),
    description,
  );
  if (!statSync(resolved).isDirectory()) {
    fail(`${description} is not a directory: ${resolved}`);
  }
  return resolved;
}

function visitTree(root, current, files) {
  for (const entry of readdirSync(current, { withFileTypes: true }).sort((a, b) =>
    a.name.localeCompare(b.name),
  )) {
    const path = resolve(current, entry.name);
    const metadata = lstatSync(path);
    if (metadata.isSymbolicLink()) {
      fail(`source tree contains a symbolic link: ${relative(root, path)}`);
    }
    if (metadata.isDirectory()) {
      visitTree(root, path, files);
      continue;
    }
    if (!metadata.isFile()) {
      fail(`source tree contains an unsupported entry: ${relative(root, path)}`);
    }
    files.push({
      path,
      relativePath: relative(root, path).replaceAll("\\", "/"),
      bytes: metadata.size,
      sha256: sha256(path),
    });
  }
}

export function describeTree(root) {
  const canonicalRoot = canonicalExistingPath(root, "source tree");
  if (!statSync(canonicalRoot).isDirectory()) {
    fail(`source tree is not a directory: ${root}`);
  }
  const files = [];
  visitTree(canonicalRoot, canonicalRoot, files);
  files.sort((left, right) => left.relativePath.localeCompare(right.relativePath));

  const digest = createHash("sha256");
  let bytes = 0;
  for (const file of files) {
    digest.update(file.relativePath, "utf8");
    digest.update("\0");
    digest.update(file.sha256, "ascii");
    digest.update("\n");
    bytes += file.bytes;
  }
  return {
    files,
    fileCount: files.length,
    bytes,
    sha256: digest.digest("hex"),
  };
}

export function assertTreesEqual(expectedRoot, actualRoot) {
  const expected = describeTree(expectedRoot);
  const actual = describeTree(actualRoot);
  const expectedFiles = new Map(
    expected.files.map((file) => [file.relativePath, file.sha256]),
  );
  const actualFiles = new Map(
    actual.files.map((file) => [file.relativePath, file.sha256]),
  );

  for (const [path, hash] of expectedFiles) {
    if (!actualFiles.has(path)) fail(`generated artifact is missing: ${path}`);
    if (actualFiles.get(path) !== hash) fail(`generated artifact is stale: ${path}`);
  }
  for (const path of actualFiles.keys()) {
    if (!expectedFiles.has(path)) fail(`unexpected generated artifact: ${path}`);
  }
}

export function replaceDirectoryFromStaging(stagingRoot, destinationRoot) {
  const staging = resolve(stagingRoot);
  const destination = resolve(destinationRoot);
  if (dirname(staging) !== dirname(destination) || staging === destination) {
    fail("staging and destination directories must be distinct siblings");
  }
  if (!existsSync(staging) || !lstatSync(staging).isDirectory()) {
    fail(`staging directory does not exist: ${staging}`);
  }
  if (existsSync(destination) && lstatSync(destination).isSymbolicLink()) {
    fail(`refusing to replace a symbolic-link destination: ${destination}`);
  }

  const backup = `${destination}.backup-${randomUUID()}`;
  let movedDestination = false;
  try {
    if (existsSync(destination)) {
      renameSync(destination, backup);
      movedDestination = true;
    }
    renameSync(staging, destination);
  } catch (error) {
    if (movedDestination && !existsSync(destination) && existsSync(backup)) {
      renameSync(backup, destination);
    }
    throw error;
  }
  if (movedDestination) {
    rmSync(backup, { force: true, recursive: true });
  }
}

export function normalizeCargoLicenseExpression(expression) {
  if (typeof expression !== "string") {
    return { expression, declaredLicense: undefined };
  }
  const compactExpression = expression.trim().replace(/\s*\/\s*/g, "/");
  const normalizedExpression = cargoLegacyLicenseExpressions.get(compactExpression);
  if (normalizedExpression === undefined) {
    return { expression, declaredLicense: undefined };
  }
  return {
    expression: normalizedExpression,
    declaredLicense: expression,
  };
}

export function validateLicenseExpression(
  expression,
  packageName,
  allowedLicenseIds,
  allowedLicenseExceptions,
) {
  if (typeof expression !== "string" || expression.trim().length === 0) {
    fail(`${packageName} has no SPDX license expression`);
  }

  let parsed;
  try {
    parsed = parseSpdxExpression(expression);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    fail(`${packageName} has an invalid SPDX license expression (${expression}): ${detail}`);
  }

  const identifiers = new Set();
  const visit = (node) => {
    if (typeof node?.license === "string") {
      if (node.plus === true) {
        fail(`${packageName} uses an unapproved or-later license (${expression})`);
      }
      if (!allowedLicenseIds.has(node.license)) {
        fail(
          `${packageName} uses unapproved license identifier ${node.license} ` +
            `(${expression})`,
        );
      }
      if (
        node.exception !== undefined &&
        !allowedLicenseExceptions.has(node.exception)
      ) {
        fail(
          `${packageName} uses unapproved license exception ${node.exception} ` +
            `(${expression})`,
        );
      }
      identifiers.add(node.license);
      return;
    }
    if (
      (node?.conjunction !== "and" && node?.conjunction !== "or") ||
      node.left === undefined ||
      node.right === undefined
    ) {
      fail(`${packageName} has an unsupported SPDX expression (${expression})`);
    }
    visit(node.left);
    visit(node.right);
  };
  visit(parsed);
  return identifiers;
}
