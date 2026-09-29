export interface CommandRow {
  do: string;
  undo: string;
  elevated?: boolean;
  [key: string]: unknown;
}

export interface ServerCommandRow {
  name: string;
  cmd: string;
  elevated?: boolean;
  [key: string]: unknown;
}

export interface DisplayFieldVisibility {
  physical: boolean;
  virtual: boolean;
}

function arrayValue(value: unknown): unknown[] {
  if (Array.isArray(value)) return value;
  if (typeof value !== 'string' || !value.trim()) return [];
  try {
    const parsed: unknown = JSON.parse(value);
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

export function normalizeCommandRows(value: unknown, platform: string): CommandRow[] {
  const entries = arrayValue(value);
  const windows = platform.toLocaleLowerCase().includes('windows');
  return entries.map((entry) => {
    const source =
      entry && typeof entry === 'object' && !Array.isArray(entry)
        ? (entry as Record<string, unknown>)
        : {};
    const row: CommandRow = {
      ...source,
      do: typeof source.do === 'string' ? source.do : String(source.do ?? ''),
      undo: typeof source.undo === 'string' ? source.undo : String(source.undo ?? ''),
    };
    if (windows) row.elevated = source.elevated === true;
    else delete row.elevated;
    return row;
  });
}

export function normalizeServerCommandRows(value: unknown, platform: string): ServerCommandRow[] {
  const entries = arrayValue(value);
  const windows = platform.toLocaleLowerCase().includes('windows');
  return entries.map((entry) => {
    const source =
      entry && typeof entry === 'object' && !Array.isArray(entry)
        ? (entry as Record<string, unknown>)
        : {};
    const row: ServerCommandRow = {
      ...source,
      name: typeof source.name === 'string' ? source.name : String(source.name ?? ''),
      cmd: typeof source.cmd === 'string' ? source.cmd : String(source.cmd ?? ''),
    };
    if (windows) row.elevated = source.elevated === true;
    else delete row.elevated;
    return row;
  });
}

export function serializeCommandRows(value: unknown, platform: string): CommandRow[] {
  return normalizeCommandRows(value, platform).map((row) => {
    const serialized: CommandRow = {
      ...row,
      do: row.do,
      undo: row.undo,
    };
    if (platform.toLocaleLowerCase().includes('windows'))
      serialized.elevated = row.elevated === true;
    else delete serialized.elevated;
    return serialized;
  });
}

export function serializeServerCommandRows(value: unknown, platform: string): ServerCommandRow[] {
  return normalizeServerCommandRows(value, platform).map((row) => {
    const serialized: ServerCommandRow = {
      ...row,
      name: row.name,
      cmd: row.cmd,
    };
    if (platform.toLocaleLowerCase().includes('windows'))
      serialized.elevated = row.elevated === true;
    else delete serialized.elevated;
    return serialized;
  });
}

export function displayFieldVisibility(mode: unknown): DisplayFieldVisibility {
  const physical =
    String(mode ?? '')
      .trim()
      .toLocaleLowerCase() === 'disabled';
  return { physical, virtual: !physical };
}

export function preserveHiddenDisplayValues(
  previous: Record<string, unknown>,
  patch: Record<string, unknown>,
): Record<string, unknown> {
  return { ...previous, ...patch };
}
