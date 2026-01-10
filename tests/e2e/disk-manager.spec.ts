import { expect, test } from '@playwright/test';

test.describe('web disk image manager', () => {
  test.beforeEach(async ({ page }) => {
    // Use the Vite dev server so we can import source modules under /web/.
    await page.goto('http://127.0.0.1:5173/', { waitUntil: 'load' });
    await page.evaluate(async () => {
      const { DiskManager } = await import('/web/src/storage/disk_manager.ts');
      await DiskManager.clearAllStorage();
    });
  });

  test('create blank disk and verify size', async ({ page }) => {
    const result = await page.evaluate(async () => {
      const { DiskManager } = await import('/web/src/storage/disk_manager.ts');
      const dm = await DiskManager.create();
      const meta = await dm.createBlankDisk({ name: 'blank', sizeBytes: 1024 * 1024 });
      const stat = await dm.statDisk(meta.id);
      dm.close();
      return { meta, stat };
    });

    expect(result.meta.sizeBytes).toBe(1024 * 1024);
    expect(result.stat.actualSizeBytes).toBe(1024 * 1024);
    expect(result.stat.meta.id).toBe(result.meta.id);
  });

  test('import small image and verify metadata + checksum', async ({ page }) => {
    const result = await page.evaluate(async () => {
      const { DiskManager } = await import('/web/src/storage/disk_manager.ts');
      const { crc32Final, crc32Init, crc32ToHex, crc32Update } = await import('/web/src/storage/crc32.ts');

      const dm = await DiskManager.create();
      const bytes = new Uint8Array(32 * 1024);
      for (let i = 0; i < bytes.length; i++) bytes[i] = (i * 31) & 0xff;
      const file = new File([bytes], 'tiny.img', { type: 'application/octet-stream' });

      const meta = await dm.importDisk(file, { name: 'tiny' });
      const disks = await dm.listDisks();

      let crc = crc32Init();
      crc = crc32Update(crc, bytes);
      const expected = crc32ToHex(crc32Final(crc));

      dm.close();
      return { meta, disks, expected };
    });

    expect(result.meta.name).toBe('tiny');
    expect(result.meta.sizeBytes).toBe(32 * 1024);
    expect(result.meta.checksum.algorithm).toBe('crc32');
    expect(result.meta.checksum.value).toBe(result.expected);
    expect(result.disks.find((d: any) => d.id === result.meta.id)).toBeTruthy();
  });

  test('export returns expected checksum/content', async ({ page }) => {
    const result = await page.evaluate(async () => {
      const { DiskManager } = await import('/web/src/storage/disk_manager.ts');
      const { crc32Final, crc32Init, crc32ToHex, crc32Update } = await import('/web/src/storage/crc32.ts');

      const dm = await DiskManager.create();
      const bytes = new Uint8Array(128 * 1024);
      for (let i = 0; i < bytes.length; i++) bytes[i] = (i ^ (i >>> 3)) & 0xff;
      const file = new File([bytes], 'export-me.img', { type: 'application/octet-stream' });

      const meta = await dm.importDisk(file, { name: 'export-me' });
      const handle = await dm.exportDiskStream(meta.id);
      const exported = new Uint8Array(await new Response(handle.stream).arrayBuffer());
      const done = await handle.done;

      let crc = crc32Init();
      crc = crc32Update(crc, bytes);
      const expected = crc32ToHex(crc32Final(crc));

      let crcExported = crc32Init();
      crcExported = crc32Update(crcExported, exported);
      const exportedCrc = crc32ToHex(crc32Final(crcExported));

      dm.close();
      return { meta, expected, exportedCrc, done };
    });

    expect(result.meta.checksum.value).toBe(result.expected);
    expect(result.exportedCrc).toBe(result.expected);
    expect(result.done.checksumCrc32).toBe(result.expected);
  });

  test('mount config supports one HDD + one CD', async ({ page }) => {
    const result = await page.evaluate(async () => {
      const { DiskManager } = await import('/web/src/storage/disk_manager.ts');

      const dm = await DiskManager.create();

      const hddFile = new File([new Uint8Array([1, 2, 3])], 'disk.img', { type: 'application/octet-stream' });
      const cdFile = new File([new Uint8Array([4, 5, 6])], 'install.iso', { type: 'application/octet-stream' });

      const hdd = await dm.importDisk(hddFile);
      const cd = await dm.importDisk(cdFile);

      await dm.setMounts({ hddId: hdd.id, cdId: cd.id });
      const mounts = await dm.getMounts();
      dm.close();
      return { hdd, cd, mounts };
    });

    expect(result.hdd.kind).toBe('hdd');
    expect(result.cd.kind).toBe('cd');
    expect(result.mounts.hddId).toBe(result.hdd.id);
    expect(result.mounts.cdId).toBe(result.cd.id);
  });
});

