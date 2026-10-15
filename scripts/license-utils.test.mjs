import assert from "node:assert/strict";
import {
  mkdtempSync,
  mkdirSync,
  realpathSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import {
  assertTreesEqual,
  normalizeCargoLicenseExpression,
  resolveContainedFile,
  validateLicenseExpression,
} from "./license-utils.mjs";
import {
  bumpReleaseVersion,
  resolveNextVersion,
  stripLeadingDoubleDash,
  verifyReleaseVersion,
} from "./check-release-version.mjs";

const allowedIds = new Set(["Apache-2.0", "MIT", "MPL-2.0"]);
const allowedExceptions = new Set(["LLVM-exception"]);

test("only allowlisted Cargo slash expressions are normalized", () => {
  assert.deepEqual(normalizeCargoLicenseExpression("Apache-2.0 / MIT"), {
    expression: "Apache-2.0 OR MIT",
    declaredLicense: "Apache-2.0 / MIT",
  });
  const unknownExpression = normalizeCargoLicenseExpression("BSD-2-Clause/MIT");
  assert.deepEqual(unknownExpression, {
    expression: "BSD-2-Clause/MIT",
    declaredLicense: undefined,
  });
  assert.throws(
    () =>
      validateLicenseExpression(
        unknownExpression.expression,
        "fixture",
        new Set(["BSD-2-Clause", "MIT"]),
        allowedExceptions,
      ),
    /invalid SPDX license expression/,
  );
});

test("SPDX expressions are parsed instead of tokenized", () => {
  const ids = validateLicenseExpression(
    "(MIT OR Apache-2.0 WITH LLVM-exception) AND MPL-2.0",
    "fixture",
    allowedIds,
    allowedExceptions,
  );
  assert.deepEqual([...ids].sort(), ["Apache-2.0", "MIT", "MPL-2.0"]);
  assert.throws(
    () =>
      validateLicenseExpression(
        "MIT trailing-text",
        "fixture",
        allowedIds,
        allowedExceptions,
      ),
    /invalid SPDX license expression/,
  );
  assert.throws(
    () =>
      validateLicenseExpression(
        "GPL-3.0-only",
        "fixture",
        allowedIds,
        allowedExceptions,
      ),
    /unapproved license identifier/,
  );
});

test("declared files cannot escape their real package root", () => {
  const sandbox = mkdtempSync(join(tmpdir(), "xmic-license-paths-"));
  const packageRoot = join(sandbox, "package");
  const outsideRoot = join(sandbox, "outside");
  const licensePath = join(packageRoot, "LICENSE");
  mkdirSync(packageRoot);
  mkdirSync(outsideRoot);
  writeFileSync(licensePath, "inside\n");
  writeFileSync(join(outsideRoot, "LICENSE"), "outside\n");

  assert.equal(
    resolveContainedFile(packageRoot, "LICENSE", "declared license"),
    realpathSync.native(licensePath),
  );
  assert.throws(
    () => resolveContainedFile(packageRoot, "../outside/LICENSE", "declared license"),
    /escapes its allowed root/,
  );
  assert.throws(
    () =>
      resolveContainedFile(
        packageRoot,
        join(outsideRoot, "LICENSE"),
        "declared license",
      ),
    /must not be absolute/,
  );

  symlinkSync(
    outsideRoot,
    join(packageRoot, "linked-directory"),
    process.platform === "win32" ? "junction" : "dir",
  );
  assert.throws(
    () =>
      resolveContainedFile(
        packageRoot,
        "linked-directory/LICENSE",
        "declared license",
      ),
    /escapes its allowed root/,
  );
});

test("generated trees are compared by exact path and content", () => {
  const sandbox = mkdtempSync(join(tmpdir(), "xmic-license-trees-"));
  const expected = join(sandbox, "expected");
  const actual = join(sandbox, "actual");
  mkdirSync(expected);
  mkdirSync(actual);
  writeFileSync(join(expected, "NOTICE"), "same\n");
  writeFileSync(join(actual, "NOTICE"), "same\n");
  assert.doesNotThrow(() => assertTreesEqual(expected, actual));

  writeFileSync(join(actual, "NOTICE"), "changed\n");
  assert.throws(() => assertTreesEqual(expected, actual), /stale/);
  writeFileSync(join(actual, "NOTICE"), "same\n");
  writeFileSync(join(actual, "EXTRA"), "unexpected\n");
  assert.throws(() => assertTreesEqual(expected, actual), /unexpected/);
});

test("release version bump updates all five manifests/locks and detects drift", () => {
  assert.deepEqual(stripLeadingDoubleDash(["--", "patch"]), ["patch"]);
  assert.deepEqual(stripLeadingDoubleDash(["patch"]), ["patch"]);
  assert.equal(resolveNextVersion("0.4.0", "patch"), "0.4.1");
  assert.equal(resolveNextVersion("0.4.0", "minor"), "0.5.0");
  assert.equal(resolveNextVersion("0.4.0", "major"), "1.0.0");
  assert.equal(resolveNextVersion("0.4.0", "0.6.2"), "0.6.2");
  assert.equal(resolveNextVersion("0.4.0", "v0.6.2"), "0.6.2");
  assert.throws(() => resolveNextVersion("0.4.0", "invalid"), /invalid target version/);

  const sandbox = mkdtempSync(join(tmpdir(), "xmic-version-bump-"));
  mkdirSync(join(sandbox, "src-tauri"), { recursive: true });
  mkdirSync(join(sandbox, "crates", "engine-core"), { recursive: true });

  writeFileSync(
    join(sandbox, "Cargo.toml"),
    `[workspace]\nmembers = [\n  "crates/engine-core",\n  "src-tauri",\n]\n\n[workspace.package]\nversion = "0.4.0"\nedition = "2021"\n`,
  );
  writeFileSync(
    join(sandbox, "crates", "engine-core", "Cargo.toml"),
    `[package]\nname = "xmic-engine-core"\nversion.workspace = true\n`,
  );
  writeFileSync(
    join(sandbox, "src-tauri", "Cargo.toml"),
    `[package]\nname = "xmic"\nversion.workspace = true\n`,
  );
  writeFileSync(
    join(sandbox, "Cargo.lock"),
    `[[package]]\nname = "serde"\nversion = "1.0.0"\nsource = "registry+https://github.com/rust-lang/crates.io-index"\n\n[[package]]\nname = "xmic"\nversion = "0.4.0"\ndependencies = [\n "xmic-engine-core",\n]\n\n[[package]]\nname = "xmic-engine-core"\nversion = "0.4.0"\n`,
  );
  writeFileSync(
    join(sandbox, "package.json"),
    `${JSON.stringify({ name: "xmic", version: "0.4.0" }, null, 2)}\n`,
  );
  writeFileSync(
    join(sandbox, "package-lock.json"),
    `${JSON.stringify(
      {
        name: "xmic",
        version: "0.4.0",
        packages: { "": { name: "xmic", version: "0.4.0" } },
      },
      null,
      2,
    )}\n`,
  );
  writeFileSync(
    join(sandbox, "src-tauri", "tauri.conf.json"),
    `${JSON.stringify({ productName: "XMic", version: "0.4.0" }, null, 2)}\n`,
  );

  assert.equal(verifyReleaseVersion(sandbox, "v0.4.0"), "0.4.0");
  const bumped = bumpReleaseVersion("minor", sandbox);
  assert.deepEqual(bumped, { previousVersion: "0.4.0", nextVersion: "0.5.0" });
  assert.equal(verifyReleaseVersion(sandbox, "v0.5.0"), "0.5.0");

  // Drift in Cargo.lock must be caught by verifyReleaseVersion.
  writeFileSync(
    join(sandbox, "Cargo.lock"),
    `[[package]]\nname = "xmic"\nversion = "0.5.0"\n\n[[package]]\nname = "xmic-engine-core"\nversion = "0.4.0"\n`,
  );
  assert.throws(
    () => verifyReleaseVersion(sandbox, null),
    /Cargo\.lock \(xmic-engine-core\)=0\.4\.0/,
  );
});
