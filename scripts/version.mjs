import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const command = process.argv[2] ?? "check";

function fail(message) {
  console.error(message);
  process.exitCode = 1;
}

function readText(relativePath) {
  return fs.readFileSync(path.join(root, relativePath), "utf8");
}

function writeText(relativePath, contents) {
  fs.writeFileSync(path.join(root, relativePath), contents);
}

function tomlSection(contents, sectionName) {
  const escaped = sectionName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const match = contents.match(
    new RegExp(
      `^\\[${escaped}\\]\\s*\\r?\\n([\\s\\S]*?)(?=^\\[|(?![\\s\\S]))`,
      "m",
    ),
  );
  if (!match) {
    throw new Error(`missing [${sectionName}] section`);
  }
  return match[1];
}

function workspaceVersion() {
  const section = tomlSection(readText("Cargo.toml"), "workspace.package");
  const match = section.match(/^\s*version\s*=\s*"([^"]+)"\s*$/m);
  if (!match) {
    throw new Error("Cargo.toml [workspace.package] must define version");
  }
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(match[1])) {
    throw new Error(`invalid semantic version: ${match[1]}`);
  }
  return match[1];
}

function workspaceMembers() {
  const section = tomlSection(readText("Cargo.toml"), "workspace");
  const members = section.match(/members\s*=\s*\[([\s\S]*?)\]/)?.[1];
  if (!members) {
    throw new Error("Cargo.toml [workspace] must define members");
  }
  return [...members.matchAll(/"([^"]+)"/g)].map((match) => match[1]);
}

function workspacePackages() {
  return workspaceMembers().map((member) => {
    const manifest = path.posix.join(member.replaceAll("\\", "/"), "Cargo.toml");
    const section = tomlSection(readText(manifest), "package");
    const name = section.match(/^\s*name\s*=\s*"([^"]+)"\s*$/m)?.[1];
    if (!name) {
      throw new Error(`${manifest} [package] must define name`);
    }
    return { manifest, name };
  });
}

function cargoLockPackageBlocks(contents) {
  const starts = [...contents.matchAll(/^\[\[package\]\]\s*$/gm)].map(
    (match) => match.index,
  );
  return starts.map((start, index) => {
    const end = starts[index + 1] ?? contents.length;
    return { start, end, contents: contents.slice(start, end) };
  });
}

function cargoLockWorkspaceVersions(contents, packages) {
  const packageNames = new Set(packages.map(({ name }) => name));
  const versions = new Map();
  for (const block of cargoLockPackageBlocks(contents)) {
    const name = block.contents.match(/^name\s*=\s*"([^"]+)"\s*$/m)?.[1];
    if (
      !name ||
      !packageNames.has(name) ||
      /^source\s*=/m.test(block.contents)
    ) {
      continue;
    }
    if (versions.has(name)) {
      throw new Error(`Cargo.lock contains multiple local packages named ${name}`);
    }
    const version = block.contents.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
    if (!version) {
      throw new Error(`Cargo.lock package ${name} is missing version`);
    }
    versions.set(name, version);
  }
  for (const { name } of packages) {
    if (!versions.has(name)) {
      throw new Error(`Cargo.lock is missing workspace package ${name}`);
    }
  }
  return versions;
}

function synchronizeCargoLock(version, packages) {
  const contents = readText("Cargo.lock");
  const packageNames = new Set(packages.map(({ name }) => name));
  const seen = new Set();
  let cursor = 0;
  let updated = "";

  for (const block of cargoLockPackageBlocks(contents)) {
    updated += contents.slice(cursor, block.start);
    let blockContents = block.contents;
    const name = blockContents.match(/^name\s*=\s*"([^"]+)"\s*$/m)?.[1];
    if (name && packageNames.has(name) && !/^source\s*=/m.test(blockContents)) {
      if (seen.has(name)) {
        throw new Error(`Cargo.lock contains multiple local packages named ${name}`);
      }
      seen.add(name);
      blockContents = blockContents.replace(
        /^version\s*=\s*"[^"]+"\s*$/m,
        `version = "${version}"`,
      );
    }
    updated += blockContents;
    cursor = block.end;
  }
  updated += contents.slice(cursor);

  for (const { name } of packages) {
    if (!seen.has(name)) {
      throw new Error(`Cargo.lock is missing workspace package ${name}`);
    }
  }
  if (updated !== contents) {
    writeText("Cargo.lock", updated);
  }
}

function packageUsesWorkspaceVersion(relativePath) {
  const section = tomlSection(readText(relativePath), "package");
  return /^\s*version\.workspace\s*=\s*true\s*$/m.test(section);
}

function synchronizeMemberVersion(relativePath) {
  const contents = readText(relativePath);
  const sectionPattern =
    /^\[package\]\s*\r?\n([\s\S]*?)(?=^\[|(?![\s\S]))/m;
  const match = contents.match(sectionPattern);
  if (!match) {
    throw new Error(`${relativePath} is missing [package]`);
  }
  let section = match[0];
  if (/^\s*version\.workspace\s*=\s*true\s*$/m.test(section)) {
    return false;
  }
  if (/^\s*version\s*=\s*"[^"]+"\s*$/m.test(section)) {
    section = section.replace(
      /^\s*version\s*=\s*"[^"]+"\s*$/m,
      "version.workspace = true",
    );
  } else {
    section = section.replace(
      /^(name\s*=\s*"[^"]+"\s*)$/m,
      "$1\nversion.workspace = true",
    );
  }
  writeText(relativePath, contents.replace(sectionPattern, section));
  return true;
}

function readJson(relativePath) {
  return JSON.parse(readText(relativePath));
}

function writeJson(relativePath, value) {
  writeText(relativePath, `${JSON.stringify(value, null, 2)}\n`);
}

function requestedTag() {
  const inline = process.argv.find((argument) => argument.startsWith("--tag="));
  if (inline) {
    return inline.slice("--tag=".length);
  }
  const index = process.argv.indexOf("--tag");
  return index >= 0 ? process.argv[index + 1] : undefined;
}

function check(version) {
  const errors = [];
  const packageJson = readJson("package.json");
  const packageLock = readJson("package-lock.json");
  const tauriConfig = readJson("src-tauri/tauri.conf.json");
  const packages = workspacePackages();

  for (const [label, actual] of [
    ["package.json", packageJson.version],
    ["package-lock.json", packageLock.version],
    ["package-lock.json packages['']", packageLock.packages?.[""]?.version],
    ["src-tauri/tauri.conf.json", tauriConfig.version],
  ]) {
    if (actual !== version) {
      errors.push(`${label} has ${JSON.stringify(actual)}, expected ${version}`);
    }
  }

  for (const { manifest } of packages) {
    if (!packageUsesWorkspaceVersion(manifest)) {
      errors.push(`${manifest} must set version.workspace = true`);
    }
  }

  const cargoLockVersions = cargoLockWorkspaceVersions(
    readText("Cargo.lock"),
    packages,
  );
  for (const { name } of packages) {
    const actual = cargoLockVersions.get(name);
    if (actual !== version) {
      errors.push(
        `Cargo.lock package ${name} has ${JSON.stringify(actual)}, expected ${version}`,
      );
    }
  }

  const tag = requestedTag();
  if (tag && tag !== `v${version}`) {
    errors.push(`release tag ${tag} does not match v${version}`);
  }

  if (errors.length > 0) {
    for (const error of errors) {
      console.error(`version mismatch: ${error}`);
    }
    process.exitCode = 1;
    return;
  }
  console.log(`version ${version} is synchronized`);
}

function sync(version) {
  const packageJson = readJson("package.json");
  const packageLock = readJson("package-lock.json");
  const tauriConfig = readJson("src-tauri/tauri.conf.json");
  const packages = workspacePackages();

  packageJson.version = version;
  packageLock.version = version;
  packageLock.packages ??= {};
  packageLock.packages[""] ??= {};
  packageLock.packages[""].version = version;
  tauriConfig.version = version;

  writeJson("package.json", packageJson);
  writeJson("package-lock.json", packageLock);
  writeJson("src-tauri/tauri.conf.json", tauriConfig);
  for (const { manifest } of packages) {
    synchronizeMemberVersion(manifest);
  }
  synchronizeCargoLock(version, packages);
  console.log(`synchronized mirrored versions to ${version}`);
}

try {
  const version = workspaceVersion();
  if (command === "print") {
    console.log(version);
  } else if (command === "check") {
    check(version);
  } else if (command === "sync") {
    sync(version);
    check(version);
  } else {
    fail(`usage: node scripts/version.mjs [check|sync|print] [--tag vX.Y.Z]`);
  }
} catch (error) {
  fail(error instanceof Error ? error.message : String(error));
}
