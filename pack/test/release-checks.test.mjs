import assert from 'node:assert/strict';
import { test } from 'node:test';
import { checkReleaseTree } from '../release-checks.mjs';

test('Web backend tests are rejected from release trees', () => {
  assert.throws(
    () => checkReleaseTree('.', ['agent/home/extensions/web/test/http.test.ts']),
    /Web 测试文件不得进入发行包/,
  );
});
