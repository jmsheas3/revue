// Builds the debug binary once before any test runs. The tests need a debug build: REVUE_TEST=1 (GET /api/debug)
// only works when debug_assertions are on.
import { spawnSync } from 'node:child_process';
import { REPO_ROOT } from './fixtures';

export default function globalSetup() {
  const r = spawnSync('cargo', ['build', '--bin', 'revue'], { cwd: REPO_ROOT, stdio: 'inherit' });
  if (r.error) throw new Error(`could not run cargo (is Rust installed?): ${r.error.message}`);
  if (r.status !== 0) throw new Error(`cargo build failed with exit code ${r.status}; fix the build before running the UI tests`);
}
