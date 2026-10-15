import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import {
  bumpReleaseVersion,
  stripLeadingDoubleDash,
} from "./check-release-version.mjs";

const isMain =
  process.argv[1] &&
  resolve(process.argv[1]) === fileURLToPath(import.meta.url);

if (isMain) {
  try {
    const args = stripLeadingDoubleDash(process.argv.slice(2));
    if (args.length !== 1) {
      throw new Error(
        "Usage: npm run version:bump -- <MAJOR.MINOR.PATCH|patch|minor|major>",
      );
    }
    const { previousVersion, nextVersion } = bumpReleaseVersion(args[0]);
    console.log(
      `Updated release version: ${previousVersion} -> ${nextVersion} (Cargo.toml, Cargo.lock, package.json, package-lock.json, src-tauri/tauri.conf.json)`,
    );
  } catch (error) {
    console.error(error instanceof Error ? error.message : error);
    process.exitCode = 1;
  }
}
