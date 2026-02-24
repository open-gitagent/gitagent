import { readFileSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));

function getSchemaDir(): string {
  // In dist/utils/, schemas are at ../../spec/schemas/
  return join(__dirname, '..', '..', 'spec', 'schemas');
}

export function loadSchema(name: string): object {
  const schemaPath = join(getSchemaDir(), `${name}.schema.json`);
  return JSON.parse(readFileSync(schemaPath, 'utf-8'));
}
