import { spawn } from "node:child_process";
import path from "node:path";

const root = path.resolve(import.meta.dirname, "..");
const executable = process.platform === "win32" ? "tauri.cmd" : "tauri";
const tauri = path.join(root, "node_modules", ".bin", executable);

const child = spawn(tauri, ["dev"], {
  cwd: root,
  env: {
    ...process.env,
    LIOS_HOME: path.join(root, ".dev-home")
  },
  stdio: "inherit"
});

child.on("error", (error) => {
  console.error(`failed to start Lios Desktop: ${error.message}`);
  process.exitCode = 1;
});

child.on("exit", (code, signal) => {
  if (signal) {
    process.kill(process.pid, signal);
    return;
  }
  process.exitCode = code ?? 1;
});
