// The fixtures themselves. Git exports GIT_DIR, GIT_INDEX_FILE and GIT_WORK_TREE to the hooks it runs, so tests
// started from one (npm test in a pre-commit hook) must keep them away from the fixtures' git and from the servers
// (cleanEnv), or `git add -A` and `commit` land in the user's repo. Here the "user's repo" is a throwaway victim.
import { test, expect, Repo } from '../fixtures';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';

// No page is involved, so one browser is enough.
test.skip(({ browserName }) => browserName !== 'chromium', 'checks the fixtures and the server only');

test('git variables from a hook reach neither the fixtures nor the servers', async ({ repo, revue }) => {
  const victimDir = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'revue-ui-victim-')));
  try {
    const victim = new Repo(victimDir).init();
    victim.write('real.txt', 'real\n').commit('victim');
    victim.write('staged.txt', 'staged\n').add('staged.txt');
    victim.write('unstaged.txt', 'not added\n');
    const index = () => fs.readFileSync(victim.abs('.git/index'));
    const head = victim.rev('HEAD'), before = index();

    // As a hook would see them, for as long as the fixtures run (this worker runs one test at a time).
    const hook = { GIT_DIR: victim.abs('.git'), GIT_INDEX_FILE: victim.abs('.git/index'), GIT_WORK_TREE: victimDir };
    const saved = Object.fromEntries(Object.keys(hook).map(k => [k, process.env[k]]));
    Object.assign(process.env, hook);
    let s;
    try {
      repo.write('fixture.txt', 'fixture\n');
      repo.commit('fixture');
      repo.write('fixture.txt', 'changed\n');
      s = await revue.start();
    } finally {
      for (const [k, v] of Object.entries(saved)) {
        if (v === undefined) delete process.env[k];
        else process.env[k] = v;
      }
    }

    expect(victim.rev('HEAD'), 'a commit landed in the victim repo').toBe(head);
    expect(index().equals(before), "the victim's index was rewritten").toBe(true);
    expect(victim.git('ls-files')).toBe('real.txt\nstaged.txt\n');
    expect(repo.git('log', '--format=%s')).toBe('fixture\n');
    expect(repo.git('ls-files')).toBe('fixture.txt\n');
    // The server was started under the hook's variables too; it must show the fixture repo.
    const tree = await s.json('/api/tree', { mode: 'working' });
    expect(tree.files).toEqual(['fixture.txt']);
    expect(tree.changes['fixture.txt']).toMatchObject({ status: 'M', add: 1, del: 1 });
  } finally {
    fs.rmSync(victimDir, { recursive: true, force: true });
  }
});
