import { cp, mkdir } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const release = process.argv.includes("--release");

function rustHostTarget() {
  const result = spawnSync("rustc", ["-vV"], { encoding: "utf8" });
  if (result.status !== 0) {
    throw new Error("failed to determine the Rust host target");
  }
  const host = result.stdout
    .split("\n")
    .find((line) => line.startsWith("host: "))
    ?.slice("host: ".length)
    .trim();
  if (!host) {
    throw new Error("rustc did not report a host target");
  }
  return host;
}

const requestedTarget = process.env.TAURI_ENV_TARGET_TRIPLE || null;
const artifactTarget = requestedTarget || rustHostTarget();
const executableSuffix = artifactTarget.includes("windows") ? ".exe" : "";
const profile = release ? "release" : "debug";
const cargoArgs = [
  "build",
  "--locked",
  "-p",
  "lios-cli",
  "--bin",
  "lios-worker"
];
if (requestedTarget) cargoArgs.push("--target", requestedTarget);
if (release) cargoArgs.push("--release");

const build = spawnSync("cargo", cargoArgs, { stdio: "inherit" });
if (build.status !== 0) {
  process.exit(build.status ?? 1);
}

const source = path.join(
  root,
  "target",
  ...(requestedTarget ? [requestedTarget] : []),
  profile,
  `lios-worker${executableSuffix}`
);
const destination = path.join(
  root,
  "src-tauri",
  "binaries",
  `lios-worker-${artifactTarget}${executableSuffix}`
);
await mkdir(path.dirname(destination), { recursive: true });
await cp(source, destination);
