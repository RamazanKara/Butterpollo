import type { TokenScope } from '../../lib/api';

/** Scope paths are anchored regular expressions; show their placeholders readably. */
export function displayPath(path: string): string {
  return path.replace(/\[\^\/\]\+/g, '{id}').replace(/\[0-9\]\+/g, '{number}');
}

/** "GET /api/apps", one entry per method. */
export function permissions(scopes: TokenScope[]): string[] {
  return scopes.flatMap((scope) => scope.methods.map((method) => `${method} ${displayPath(scope.path)}`));
}
