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
