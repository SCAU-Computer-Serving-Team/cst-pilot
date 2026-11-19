import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { access, readFile } from "node:fs/promises";
import { test } from "node:test";

const sharedFonts = new URL("../../../../assets/fonts/", import.meta.url);
const webStaticFonts = new URL("../../../../agent/home/extensions/web/static/fonts/", import.meta.url);

const EXPECTED_VF_SHA256 = "ad10f878ea1af97d66ec8bd10eb39798784f3a1ee3d5987a570ef6083f941ef3";
const EXPECTED_VF_BYTES = 15_636_088;

function readOpenTypeTableTags(buffer) {
  assert.equal(buffer.subarray(0, 4).toString("ascii"), "OTTO");
  const numTables = buffer.readUInt16BE(4);
  const tags = new Set();
  for (let index = 0; index < numTables; index += 1) {
    const offset = 12 + index * 16;
    tags.add(buffer.subarray(offset, offset + 4).toString("ascii"));
  }
  return tags;
}

test("桌面与 Web 共用单文件原版可变字体与 OFL 许可，Web 静态目录不重复拷贝", async () => {
  const bytes = await readFile(new URL("SourceHanSansCN-VF.otf", sharedFonts));
  assert.equal(bytes.length, EXPECTED_VF_BYTES);
  assert.equal(createHash("sha256").update(bytes).digest("hex"), EXPECTED_VF_SHA256);

  const tags = readOpenTypeTableTags(bytes);
  for (const required of ["CFF2", "fvar", "STAT", "cmap", "name"]) {
    assert.equal(tags.has(required), true, `缺少 OpenType 表 ${required}`);
  }

  const license = await readFile(new URL("LICENSE.txt", sharedFonts), "utf8");
  assert.match(license, /SIL OPEN FONT LICENSE/);

  await assert.rejects(
    access(webStaticFonts),
    { code: "ENOENT" },
    "Web 静态目录不应重复存放字体副本",
  );
});
