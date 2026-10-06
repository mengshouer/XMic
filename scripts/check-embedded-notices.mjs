import { existsSync, readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const executablePath = join(projectRoot, "target", "release", "XMic.exe");
const distRoot = join(projectRoot, "dist");
const indexHtmlPath = join(distRoot, "index.html");

// The About page reads these at click time, so every notice has to be present in the
// frontend build. The large generated notices must stay out of the entry chunk as well,
// otherwise startup would parse megabytes it only needs when a user opens the dialog.
const sourcePaths = [
  "LICENSE",
  "THIRD_PARTY_NOTICES.md",
  "artifacts/licenses/CARGO_THIRD_PARTY_NOTICES.txt",
  "artifacts/licenses/NPM_THIRD_PARTY_NOTICES.txt",
  "artifacts/webrtc/win-x64/WEBRTC_THIRD_PARTY_NOTICES.txt",
  "third-party/licenses/MPL-2.0.txt",
  "artifacts/licenses/MPL_SOURCE_CODE.txt",
];

function fail(message) {
  throw new Error(message);
}

function requireFile(path, description) {
  if (!existsSync(path) || !statSync(path).isFile()) {
    fail(`${description} is missing: ${path}`);
  }
}

function normalizeForMatch(text) {
  // Vite emits raw imports inside template literals, so backticks, quotes, and
  // backslashes may appear escaped in the chunk while the probe is not.
  return text.replace(/[\\`]/g, "");
}

function noticeProbes(path) {
  // Single lines survive Vite's string escaping, while short lines such as
  // "MIT License" also occur in ordinary UI copy. Keep only lines that are long
  // enough to identify the notice text itself.
  const lines = readFileSync(path, "utf8")
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length >= 20);
  if (lines.length === 0) {
    fail(`notice has no distinctive line: ${relative(projectRoot, path)}`);
  }
  const probes = [lines[0]];
  for (const fraction of [0.25, 0.5, 0.75]) {
    const candidate = lines[Math.floor(lines.length * fraction)];
    if (candidate && !probes.includes(candidate)) probes.push(candidate);
  }
  return probes;
}

function collectFiles(root) {
  const files = [];
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) visit(path);
      else if (entry.isFile()) files.push(path);
    }
  };
  visit(root);
  return files;
}

function entryScriptPaths() {
  const html = readFileSync(indexHtmlPath, "utf8");
  const references = [...html.matchAll(/(?:src|href)="([^"]+\.js)"/g)].map(
    (match) => match[1].replace(/^\.?\//, ""),
  );
  if (references.length === 0) {
    fail(`frontend entry script could not be resolved from ${indexHtmlPath}`);
  }
  return new Set(references.map((reference) => join(distRoot, reference)));
}

function run() {
  requireFile(executablePath, "release executable");
  if (!existsSync(distRoot) || !statSync(distRoot).isDirectory()) {
    fail(`frontend build output is missing: ${distRoot}`);
  }
  requireFile(indexHtmlPath, "frontend entry document");

  const entryPaths = entryScriptPaths();
  const chunks = collectFiles(distRoot)
    .filter((path) => path.endsWith(".js"))
    .map((path) => ({
      path,
      content: normalizeForMatch(readFileSync(path, "utf8")),
    }));
  const eagerChunks = chunks.filter((chunk) => entryPaths.has(chunk.path));
  const lazyChunks = chunks.filter((chunk) => !entryPaths.has(chunk.path));
  if (lazyChunks.length === 0) {
    fail("frontend build contains no lazy chunks for the license notices");
  }

  for (const sourcePath of sourcePaths) {
    const absoluteSourcePath = join(projectRoot, sourcePath);
    requireFile(absoluteSourcePath, "notice source");
    for (const probe of noticeProbes(absoluteSourcePath)) {
      const marker = normalizeForMatch(probe);
      if (!chunks.some((chunk) => chunk.content.includes(marker))) {
        fail(
          `frontend build does not contain notice text from ${sourcePath}: ${probe}`,
        );
      }
      if (eagerChunks.some((chunk) => chunk.content.includes(marker))) {
        fail(
          `notice from ${sourcePath} is embedded in the entry chunk instead of a lazy chunk: ${probe}`,
        );
      }
    }
  }

  console.log(
    "Verified all seven notice sources are embedded as lazy chunks in the frontend build.",
  );
}

try {
  run();
} catch (error) {
  console.error(error instanceof Error ? error.message : error);
  process.exitCode = 1;
}
