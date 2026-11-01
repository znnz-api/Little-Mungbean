"use strict";

const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");

async function main() {
  const listeners = new Map();
  const context = vm.createContext({
    console: { ...console, warn() {} },
    location: { href: "app://-/index.html" },
    fetch: async () => { throw new Error("fetch must not be used when inline catalog exists"); },
    setTimeout: (callback) => { callback(); return 1; },
    clearTimeout: () => {},
    electronBridge: { sendMessageFromView: async () => {} },
    setInterval: () => 1,
  });
  context.window = context;
  context.addEventListener = (type, listener) => {
    const values = listeners.get(type) || [];
    values.push(listener);
    listeners.set(type, values);
  };
  context.dispatchEvent = () => true;
  context.removeEventListener = () => {};
  context.__LITTLE_MUNGBEAN_CLIENT_CONFIG__ = {
    version: "test",
    defaultModel: "gpt-5.6-sol",
    modelDescription: "From https://api.znnz.net",
    catalog: {
      models: [
        {
          slug: "gpt-5.6-sol",
          visibility: "list",
          supported_in_api: true,
          default_reasoning_level: "medium",
          supported_reasoning_levels: [
            { effort: "low", description: "Low" },
            { effort: "medium", description: "Medium" },
          ],
        },
        { slug: "gpt-5.6-sol-max", visibility: "list", supported_in_api: true },
      ],
    },
  };

  const script = fs.readFileSync(path.join(__dirname, "..", "assets", "renderer-inject.js"), "utf8");
  vm.runInContext(script, context, { filename: "renderer-inject.js" });
  await Promise.resolve();
  await Promise.resolve();

  const api = context.__LITTLE_MUNGBEAN_MODEL_UNLOCK__;
  assert.equal(api._test.modelNameForLabel("5.6 Sol"), "gpt-5.6-sol");
  assert.equal(api._test.modelNameForLabel("5.6 Sol Max"), "gpt-5.6-sol-max");
  assert.equal(api._test.modelNameForLabel("unknown model"), null);

  const leaf = (text) => ({ textContent: text, children: [] });
  const container = (text, attributes = {}) => {
    const textLeaf = leaf(text);
    const attrs = { ...attributes };
    return {
      children: [textLeaf],
      textLeaf,
      querySelectorAll: (selector) => selector === "*" ? [textLeaf] : [],
      getAttribute: (name) => attrs[name] ?? null,
      setAttribute: (name, value) => { attrs[name] = value; },
      closest: () => null,
      attrs,
    };
  };

  const pickerTrigger = container("5.6 Sol");
  pickerTrigger.closest = (selector) => selector === 'button[data-codex-intelligence-trigger="true"]'
    ? pickerTrigger
    : null;
  const modelSubmenuTrigger = container("5.6 Sol", {
    "aria-label": "模型 5.6 Sol",
    "aria-controls": "gateway-model-menu",
  });
  const allModelsTrigger = container("All models", {
    "aria-controls": "gateway-model-menu",
  });
  allModelsTrigger.closest = (selector) => selector === '[role="menuitem"][aria-haspopup="menu"][aria-controls]'
    ? allModelsTrigger
    : null;
  const solRow = container("5.6 Sol");
  const solMaxRow = container("5.6 Sol Max");
  const reasoningRow = container("高");
  const modelMenu = {
    querySelectorAll: () => [solRow, solMaxRow, reasoningRow],
  };
  for (const row of [solRow, solMaxRow, reasoningRow]) row.closest = () => modelMenu;
  const ordinaryChatText = leaf("5.6 Sol");

  context.document = {
    querySelectorAll: (selector) => {
      if (selector === 'button[data-codex-intelligence-trigger="true"]') return [pickerTrigger];
      if (selector === '[role="menuitem"][aria-haspopup="menu"][aria-controls]') return [modelSubmenuTrigger, allModelsTrigger];
      return [];
    },
    getElementById: (id) => id === "gateway-model-menu" ? modelMenu : null,
  };

  assert.equal(api._test.patchModelPickerLabels(), 3);
  assert.equal(pickerTrigger.textLeaf.textContent, "5.6 Sol", "current model trigger must keep the native label");
  assert.equal(modelSubmenuTrigger.textLeaf.textContent, "gpt-5.6-sol");
  assert.equal(modelSubmenuTrigger.attrs["aria-label"], "模型 gpt-5.6-sol");
  assert.equal(solRow.textLeaf.textContent, "gpt-5.6-sol");
  assert.equal(solMaxRow.textLeaf.textContent, "gpt-5.6-sol-max");
  assert.equal(reasoningRow.textLeaf.textContent, "高");
  assert.equal(ordinaryChatText.textContent, "5.6 Sol", "ordinary chat/task text must remain untouched");
  assert.equal(api._test.patchModelPickerLabels(), 0, "label patch must be idempotent");

  assert.equal(api.health().pickerInteractionHookInstalled, true);
  const pointerListeners = listeners.get("pointerdown") || [];
  assert.equal(pointerListeners.length, 1, "picker pointer hook must be installed exactly once");

  pickerTrigger.textLeaf.textContent = "5.6 Sol";
  modelSubmenuTrigger.textLeaf.textContent = "5.6 Sol";
  modelSubmenuTrigger.attrs["aria-label"] = "Model 5.6 Sol";
  solRow.textLeaf.textContent = "5.6 Sol";
  solMaxRow.textLeaf.textContent = "5.6 Sol Max";
  pointerListeners[0]({ target: allModelsTrigger });
  assert.equal(pickerTrigger.textLeaf.textContent, "5.6 Sol", "current model trigger must keep the native label after refresh");
  assert.equal(modelSubmenuTrigger.textLeaf.textContent, "gpt-5.6-sol");
  assert.equal(solRow.textLeaf.textContent, "gpt-5.6-sol");
  assert.equal(solMaxRow.textLeaf.textContent, "gpt-5.6-sol-max");

  solRow.textLeaf.textContent = "5.6 Sol";
  pointerListeners[0]({ target: ordinaryChatText });
  assert.equal(solRow.textLeaf.textContent, "5.6 Sol", "ordinary clicks must not scan or patch the picker");
  assert.equal(ordinaryChatText.textContent, "5.6 Sol", "ordinary chat/task text must remain untouched");


  console.log("renderer labels regression tests passed");
}
main().catch(error => { console.error(error); process.exitCode = 1; });
