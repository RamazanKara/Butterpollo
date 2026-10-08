// Paths chosen with the console's file picker.

/** A picked program as a command: quoted when its path has spaces. */
export function commandFor(path: string): string {
  return /\s/.test(path) ? `"${path}"` : path;
}

/**
 * Where browsing starts for a field's value: the program of a command, or
 * the path itself. Empty, which lists the drives, unless the value starts
 * with an absolute Windows path; the host lists the nearest folder that
 * exists, so arguments after the program do no harm.
 */
export function startPath(value: string): string {
  const text = value.trim();
  const path = /^"([^"]*)"/.exec(text)?.[1] ?? text;
  return /^([a-z]:[\\/]|\\\\)/i.test(path) ? path : '';
}

/** The key of a cover saved in the host's covers folder, else ''. */
export function coverKey(path: string): string {
  return /[\\/]covers[\\/]([A-Za-z0-9_-]{1,128})\.png$/i.exec(path)?.[1] ?? '';
}
