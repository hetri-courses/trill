import path from "node:path";

export function trillPathOverride() {
  return (
    process.env.CODEX_EXECUTABLE ??
    path.join(process.cwd(), "..", "..", "trill-rs", "target", "debug", "trill")
  );
}
