// Pure generator regressions: checked-in artifacts only, no sibling or installed profiles.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import {
  buildClassificationTable,
  renderMarkdown,
} from '../scripts/generate-profile-compatibility-inventory.mjs';

const artifact = JSON.parse(fs.readFileSync(
  new URL('./fixtures/profile-compatibility-inventory.json', import.meta.url), 'utf8',
));
const doc = fs.readFileSync(
  new URL('../docs/profile-compatibility.md', import.meta.url), 'utf8',
);

function imageParagraph(markdown) {
  const paragraph = markdown.split('\n\n').find((p) =>
    p.startsWith('The current `astramedium` shape includes host image-resize settings.'));
  assert.ok(paragraph, 'image retention paragraph must exist');
  return paragraph;
}

test('entire generated classification table matches the checked-in inventory', () => {
  assert.deepEqual(buildClassificationTable(), artifact.classifications);
});

test('shell notes distinguish Codex policy from supported bounded non-Codex Chat', () => {
  const table = buildClassificationTable();
  for (const key of ['shell-default-timeout-seconds', 'shell-max-timeout-seconds']) {
    const row = table.find((r) => r.key === key);
    assert.equal(row.owner, 'rust-shell-executor');
    assert.match(row.note, /Codex:.*1\.\.4294967295.*-1.*omission 120/);
    assert.match(row.note, /Non-Codex Chat:.*1\.\.7200.*omission 120/);
    assert.match(row.note, /default must not exceed maximum/);
    assert.match(row.note, /-1 rejects/);
    assert.doesNotMatch(row.note, /other APIs reject|inert/);
  }
  assert.match(table.find((r) => r.key === 'shell-default-timeout-seconds').note,
    /per-call override then maximum cap/);
  assert.match(table.find((r) => r.key === 'shell-max-timeout-seconds').note,
    /independent of request\/turn deadlines/);
});

test('pure rendering preserves the intended HostImageResizeSettings paragraph', () => {
  const rendered = renderMarkdown(artifact);
  const paragraph = imageParagraph(rendered);
  assert.equal(paragraph, imageParagraph(doc));
  assert.match(paragraph, /`image-resize\.maxLongEdge`/);
  assert.match(paragraph, /`image-resize\.maxShortEdge`/);
  assert.match(paragraph, /`image-resize\.maxPixels`/);
  assert.match(paragraph, /JSON numbers retained in `HostImageResizeSettings` as external-host data/);
  assert.match(paragraph, /accepts text\/tool prompts and has no image-input\/resizing pipeline/);
  assert.match(paragraph, /not projected into provider requests/);
  assert.match(paragraph, /Non-numeric values reject/);
  assert.equal(renderMarkdown(artifact), rendered, 'rendering is deterministic');
  // Deliberately scoped to F2: unrelated pre-existing friendliglm drift is not repaired here.
});
