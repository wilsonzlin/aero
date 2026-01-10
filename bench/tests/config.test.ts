import { test } from 'node:test';
import assert from 'node:assert/strict';

import { parseBenchCli, parseDiskImageSource } from '../runner/config.ts';

test('parseDiskImageSource detects URLs', () => {
  assert.deepEqual(parseDiskImageSource('https://example.com/disk.img'), {
    kind: 'url',
    url: 'https://example.com/disk.img',
  });
});

test('parseDiskImageSource treats other strings as paths', () => {
  assert.deepEqual(parseDiskImageSource('/tmp/disk.img'), { kind: 'path', path: '/tmp/disk.img' });
});

test('parseBenchCli reads disk image from env', () => {
  const cmd = parseBenchCli(['system_boot'], { AERO_DISK_IMAGE_PATH: '/tmp/disk.img' }, new Date(0));
  assert.equal(cmd.kind, 'run');
  if (cmd.kind !== 'run') throw new Error('expected run');
  assert.deepEqual(cmd.config.diskImage, { kind: 'path', path: '/tmp/disk.img' });
});

