"use strict";
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const script = fs.readFileSync(path.join(__dirname, "../assets/renderer-inject.js"), "utf8");

function tokens(value) {
  if (value === null || typeof value !== "object") return [{ type: "value", value }];
  const array = Array.isArray(value);
  const result = [{ type: array ? "array-start" : "object-start" }];
  for (const [key, item] of Object.entries(value)) {
    if (!array) result.push({ type: "key", value: key });
    if (typeof item === "string") result.push(
      { type: "string-start", target: "value" },
      { type: "string-chunk", value: item.slice(0, 2) },
      { type: "string-chunk", value: item.slice(2) },
      { type: "string-end" },
    );
    else result.push(...tokens(item));
  }
  result.push({ type: "container-end" });
  return result;
}

async function fixture({ chunked = false, models = ["gpt-5.6-sol", "gpt-5.6-sol-max"], replies, allowEmpty = false, failure } = {}) {
  const listeners = new Map();
  const requests = [];
  const context = vm.createContext({
    console: { warn() {} }, location: { href: "app://-/index.html" },
    setTimeout, clearTimeout, setInterval: () => 1,
    fetch: async () => { throw new Error("inline catalog must not fetch"); },
  });
  context.window = context;
  context.addEventListener = (type, listener) => {
    const values = listeners.get(type) || [];
    values.push(listener); listeners.set(type, values);
  };
  context.removeEventListener = (type, listener) => {
    listeners.set(type, (listeners.get(type) || []).filter(item => item !== listener));
  };
  const emit = data => {
    for (const listener of [...(listeners.get("message") || [])]) listener({ data });
  };
  context.electronBridge = {
    sendMessageFromView: async (payload) => {
      requests.push(payload);
      queueMicrotask(() => {
        emit({ type: "mcp-response", message: { id: "unrelated", result: { data: [{ model: "native-other" }] } } });
        const page = replies?.[requests.length - 1] || { data: models.map(model => ({ model, hidden: false })), nextCursor: null };
        const response = { type: "mcp-response", message: { id: payload.request.id, ...(failure ? { error: { message: failure } } : { result: page }) } };
        if (!chunked) { emit(response); return; }
        const all = tokens(response), transferId = "fixture-" + requests.length;
        emit({ marker: "codex-host-chunked-message-v1", transferId, kind: "start", sequence: 0 });
        let sequence = 1;
        for (let index = 0; index < all.length; index += 7) emit({ marker: "codex-host-chunked-message-v1", transferId, kind: "chunk", sequence: sequence++, tokens: all.slice(index, index + 7) });
        emit({ marker: "codex-host-chunked-message-v1", transferId, kind: "end", sequence });
      });
    },
  };
  context.__LITTLE_MUNGBEAN_CLIENT_CONFIG__ = {
    version: "test", allowEmptyModelList: allowEmpty,
    catalog: { models: models.map(slug => ({ slug, visibility: "list", supported_in_api: true })) },
  };
  vm.runInContext(script, context);
  const api = context.__LITTLE_MUNGBEAN_MODEL_UNLOCK__;
  await api.refresh();
  return { context, api, requests, listeners };
}

async function main() {
  for (const chunked of [false, true]) {
    const { api, requests, listeners } = await fixture({ chunked, replies: [
      { data: [{ model: "gpt-5.6-sol", hidden: false }], nextCursor: "page-2" },
      { data: [{ model: "gpt-5.6-sol-max", hidden: false }], nextCursor: null },
    ] });
    assert.equal(api.health().modelListVerified, true);
    assert.equal(api.health().verifiedModelCount, 2);
    assert.equal(api.strategy, "native-catalog-v1");
    assert.equal(requests.length, 2);
    assert.equal(requests[1].request.params.cursor, "page-2");
    assert.ok(requests.every(item => item.request.method === "model/list" && item.hostId === "local"));
    assert.equal((listeners.get("message") || []).length, 0);
  }
  for (const chunked of [false, true]) {
    const { api } = await fixture({ chunked, replies: [{ data: [{ model: "native-other" }], nextCursor: null }] });
    assert.equal(api.health().catalogReady, true);
    assert.equal(api.health().modelListVerified, false, "catalog loading alone must not report success");
    assert.match(api.health().verificationError, /does not match/);
  }
  const empty = await fixture({ models: [], allowEmpty: true });
  assert.equal(empty.api.health().catalogReady, true);
  assert.equal(empty.api.health().modelListVerified, true);
  const duplicate = await fixture({ replies: [{ data: [
    { model: "gpt-5.6-sol" }, { model: "gpt-5.6-sol" },
  ], nextCursor: null }] });
  assert.equal(duplicate.api.health().modelListVerified, false, "duplicates must not hide a missing model");
  const failed = await fixture({ failure: "fixture failure" });
  assert.equal(failed.api.health().modelListVerified, false);
  assert.match(failed.api.health().verificationError, /fixture failure/);
  const repeated = await fixture({ replies: [
    { data: [], nextCursor: "again" }, { data: [], nextCursor: "again" },
  ] });
  assert.equal(repeated.api.health().modelListVerified, false);
  assert.match(repeated.api.health().verificationError, /repeated a cursor/);
  const { api, context, listeners } = await fixture();
  vm.runInContext(script, context);
  await api.refresh();
  assert.equal(api.health().modelListVerified, true);
  assert.equal(listeners.get("pointerdown").length, 1);
  for (const forbidden of ["webpackChunk", "getDynamicConfig", "patchModelListResult", "MutationObserver", "Response.prototype.json"]) {
    assert.equal(script.includes(forbidden), false, "one native catalog strategy: " + forbidden);
  }
  console.log("Native model catalog passed: plain/chunked responses, pagination, mismatches, empty lists, failures and reconfiguration.");
}
module.exports = { fixture };
if (require.main === module) main().catch(error => { console.error(error); process.exitCode = 1; });
