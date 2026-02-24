import { existsSync, mkdirSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { createHash } from 'node:crypto';
import { homedir, tmpdir } from 'node:os';
import { execSync } from 'node:child_process';

export interface ResolveRepoOptions {
  branch?: string;
  refresh?: boolean;
  noCache?: boolean;
}

export interface ResolveRepoResult {
  dir: string;
  cleanup?: () => void;
}

const CACHE_BASE = join(homedir(), '.gitagent', 'cache');

function cacheKey(url: string, branch: string): string {
  return createHash('sha256').update(`${url}#${branch}`).digest('hex').slice(0, 16);
}

export function resolveRepo(url: string, options: ResolveRepoOptions = {}): ResolveRepoResult {
  const branch = options.branch ?? 'main';

  if (options.noCache) {
    const dir = join(tmpdir(), `gitagent-${cacheKey(url, branch)}-${Date.now()}`);
    cloneRepo(url, branch, dir);
    return {
      dir,
      cleanup: () => {
        try { rmSync(dir, { recursive: true, force: true }); } catch { /* ignore */ }
      },
    };
  }

  const hash = cacheKey(url, branch);
  const dir = join(CACHE_BASE, hash);

  if (existsSync(dir) && !options.refresh) {
    return { dir };
  }

  if (existsSync(dir) && options.refresh) {
    execSync('git pull --depth 1', { cwd: dir, stdio: 'pipe' });
    return { dir };
  }

  cloneRepo(url, branch, dir);
  return { dir };
}

function cloneRepo(url: string, branch: string, dir: string): void {
  mkdirSync(dir, { recursive: true });
  execSync(`git clone --depth 1 --branch ${branch} ${url} ${dir}`, {
    stdio: 'pipe',
  });
}
