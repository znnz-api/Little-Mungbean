const assert = require('node:assert/strict');
const { fixture } = require('./native-model-catalog.cjs');
async function main() {
  const { api } = await fixture({ models: ['gpt-old', 'gpt-new'] });
  assert.equal(api.health().modelListVerified, true);
  api.configure({ catalog: { models: [
    { slug: 'gpt-old', visibility: 'hide', supported_in_api: true },
    { slug: 'gpt-new', visibility: 'list', supported_in_api: true },
  ] } });
  await api.refresh();
  assert.equal(api.health().modelCount, 1);
  assert.equal(api.health().modelListVerified, false, 'stale native list must fail after visibility changes');
  const empty = await fixture({ models: [], allowEmpty: true, chunked: true });
  assert.equal(empty.api.health().modelListVerified, true);
  assert.equal(empty.api.health().verifiedModelCount, 0);
  console.log('Model menu smoke passed: visibility changes and empty native catalogs.');
}
main().catch(error => { console.error(error); process.exitCode = 1; });
