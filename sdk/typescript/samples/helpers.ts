import path from "node:path";

export function stevePathOverride() {
  return (
    process.env.STEVE_EXECUTABLE ??
    path.join(process.cwd(), "..", "..", "codex-rs", "target", "debug", "steve")
  );
}
