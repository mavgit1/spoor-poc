// Shared by the extension (download names) and the service (run files).

/**
 * A relative path below a run's files/ folder, from a script (course and
 * folder names: spaces, umlauts, slashes for subfolders). Characters no file
 * system likes become "_"; empty, "." and ".." segments are dropped. Returns
 * "" if nothing usable is left.
 */
export function safeRelPath(path: string): string {
  return path
    .split(/[\\/]+/)
    .map(seg => seg.replace(/[\u0000-\u001f:*?"<>|]/g, "_").trim().replace(/^\.+$/, "").slice(0, 200))
    .filter(Boolean)
    .join("/");
}
