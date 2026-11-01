(() => {
  "use strict";

  const incoming = globalThis.__LITTLE_MUNGBEAN_CLIENT_CONFIG__ || {};
  const existingUnlock = globalThis.__LITTLE_MUNGBEAN_MODEL_UNLOCK__;
  if (existingUnlock && existingUnlock.version === incoming.version && existingUnlock.strategy === "native-catalog-v1") {
    existingUnlock.configure(incoming);
    return;
  }

  const state = {
    config: { ...incoming },
    catalog: { models: [] },
    names: [],
    hiddenNames: new Set(),
    metadata: new Map(),
    loadedAt: 0,
    loading: null,
    catalogSource: "none",
    failures: [],
    verifiedModels: [],
    verificationPending: null,
    verificationError: null,
    patchedLabelCount: 0,
    messageHookInstalled: false,
    pickerInteractionHookInstalled: false,
  };

  const logFailure = (where, error) => {
    const item = { where, message: String(error?.message || error), at: Date.now() };
    state.failures.push(item);
    if (state.failures.length > 30) state.failures.shift();
    console.warn("[little-mungbean-client]", where, item.message);
  };

  const unique = (items) => [...new Set(items
    .filter((value) => typeof value === "string" && value.trim())
    .map((value) => value.trim()))];

  const normalizeCatalog = (payload) => {
    const all = Array.isArray(payload?.models) ? payload.models : [];
    const models = all.filter((model) => model && typeof model.slug === "string"
      && model.visibility !== "hide" && model.supported_in_api !== false);
    const metadata = new Map();
    for (const model of models) metadata.set(model.slug, model);
    state.catalog = { ...payload, models };
    state.metadata = metadata;
    state.names = unique(models.map((model) => model.slug));
    state.hiddenNames = new Set(unique(payload?.little_mungbean_hidden_models || []));
    state.loadedAt = Date.now();
    return state.catalog;
  };

  const loadCatalog = async (force = false) => {
    if (!force && state.loading) return state.loading;
    if (!force && state.names.length && Date.now() - state.loadedAt < 30_000) return state.catalog;
    const inlineCatalog = state.config.catalog;
    if (inlineCatalog && Array.isArray(inlineCatalog.models)) {
      state.catalogSource = "inline";
      return normalizeCatalog(inlineCatalog);
    }
    const url = state.config.catalogUrl;
    if (!url) throw new Error("catalogUrl and inline catalog are missing");
    state.loading = fetch(url, { cache: "no-store", credentials: "omit" })
      .then((response) => {
        if (!response.ok) throw new Error(`Helper HTTP ${response.status}`);
        return response.json();
      })
      .then((payload) => {
        state.catalogSource = "helper";
        return normalizeCatalog(payload);
      })
      .finally(() => { state.loading = null; });
    return state.loading;
  };

  const modelMetadata = (name) => state.metadata.get(name) || {};

  const normalizedLabel = (value) => String(value || "").trim().replace(/\s+/g, " ").toLowerCase();

  const titleModelPart = (value) => String(value || "")
    .split(/[-_.]+/)
    .filter(Boolean)
    .map((part) => part.length ? `${part[0].toUpperCase()}${part.slice(1)}` : part)
    .join(" ");

  const modelLabelCandidates = (name) => {
    const labels = new Set([name]);
    const model = modelMetadata(name);
    for (const key of ["displayName", "display_name", "name", "title", "label"]) {
      if (typeof model[key] === "string" && model[key].trim()) labels.add(model[key].trim());
    }
    const gpt = /^gpt-(\d+(?:\.\d+)*)(?:-(.+))?$/i.exec(name);
    if (gpt) {
      const suffix = titleModelPart(gpt[2]);
      labels.add(suffix ? `${gpt[1]} ${suffix}` : gpt[1]);
      labels.add(suffix ? `GPT-${gpt[1]} ${suffix}` : `GPT-${gpt[1]}`);
    }
    return [...labels];
  };

  const modelNameForLabel = (label) => {
    const normalized = normalizedLabel(label);
    if (!normalized) return null;
    const exact = state.names.find((name) => normalizedLabel(name) === normalized);
    if (exact) return exact;
    const matches = state.names.filter((name) => modelLabelCandidates(name)
      .some((candidate) => normalizedLabel(candidate) === normalized));
    return matches.length === 1 ? matches[0] : null;
  };

  const modelTextMatch = (container) => {
    if (!container) return null;
    const leaves = [];
    if (!container.children || container.children.length === 0) leaves.push(container);
    if (typeof container.querySelectorAll === "function") {
      for (const element of container.querySelectorAll("*")) {
        if (!element.children || element.children.length === 0) leaves.push(element);
      }
    }
    for (const leaf of leaves) {
      const label = String(leaf.textContent || "").trim();
      const name = modelNameForLabel(label);
      if (name) return { leaf, label, name };
    }
    return null;
  };

  const patchModelText = (container) => {
    const match = modelTextMatch(container);
    if (!match) return null;
    if (match.label !== match.name) {
      match.leaf.textContent = match.name;
      state.patchedLabelCount += 1;
    }
    if (typeof container.getAttribute === "function" && typeof container.setAttribute === "function") {
      const ariaLabel = container.getAttribute("aria-label");
      if (ariaLabel && match.label !== match.name && ariaLabel.endsWith(match.label)) {
        container.setAttribute("aria-label", `${ariaLabel.slice(0, -match.label.length)}${match.name}`);
      }
    }
    return match.name;
  };

  const directMenuItems = (menu) => {
    if (!menu || typeof menu.querySelectorAll !== "function") return [];
    return [...menu.querySelectorAll('[role="menuitem"], [role="menuitemradio"], [role="option"]')]
      .filter((item) => typeof item.closest !== "function" || item.closest('[role="menu"]') === menu);
  };

  const patchModelPickerLabels = () => {
    if (typeof document === "undefined" || !state.names.length) return 0;
    const before = state.patchedLabelCount;
    const submenuIds = new Set();
    for (const item of document.querySelectorAll('[role="menuitem"][aria-haspopup="menu"][aria-controls]')) {
      patchModelText(item);
      const controls = item.getAttribute("aria-controls");
      if (controls) submenuIds.add(controls);
    }

    for (const id of submenuIds) {
      const menu = typeof document.getElementById === "function" ? document.getElementById(id) : null;
      for (const item of directMenuItems(menu)) patchModelText(item);
    }
    return state.patchedLabelCount - before;
  };

  const interactionElement = (target) => {
    if (!target) return null;
    if (typeof target.closest === "function") return target;
    return target.parentElement && typeof target.parentElement.closest === "function"
      ? target.parentElement
      : null;
  };

  const isModelPickerInteraction = (target) => {
    const element = interactionElement(target);
    if (!element) return false;
    if (element.closest('button[data-codex-intelligence-trigger="true"]')) return true;
    const submenuTrigger = element.closest('[role="menuitem"][aria-haspopup="menu"][aria-controls]');
    return Boolean(submenuTrigger);
  };

  let pickerRefreshWindowUntil = 0;
  const schedulePickerLabelRefresh = () => {
    const now = Date.now();
    if (now < pickerRefreshWindowUntil) return false;
    pickerRefreshWindowUntil = now + 100;
    const patch = () => {
      try { patchModelPickerLabels(); } catch (error) { logFailure("model-labels-event", error); }
    };
    if (typeof queueMicrotask === "function") queueMicrotask(patch);
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(patch);
    for (const delay of [0, 16, 40, 80, 150, 300, 600]) setTimeout(patch, delay);
    return true;
  };

  const installPickerInteractionPatch = () => {
    if (window.__LITTLE_MUNGBEAN_PICKER_INTERACTION_PATCHED__) {
      state.pickerInteractionHookInstalled = true;
      return;
    }
    window.__LITTLE_MUNGBEAN_PICKER_INTERACTION_PATCHED__ = true;
    state.pickerInteractionHookInstalled = true;
    const handlePointer = (event) => {
      if (isModelPickerInteraction(event?.target)) schedulePickerLabelRefresh();
    };
    const handleKeydown = (event) => {
      if (!["Enter", " ", "Spacebar", "ArrowDown"].includes(String(event?.key || ""))) return;
      if (isModelPickerInteraction(event?.target)) schedulePickerLabelRefresh();
    };
    window.addEventListener("pointerdown", handlePointer, true);
    window.addEventListener("click", handlePointer, true);
    window.addEventListener("keydown", handleKeydown, true);
  };

  // The CLI reads model_catalog_json before startup. Observe only our own
  // read-only model/list requests; never rewrite application RPCs or feature flags.
  const readModelPage = (cursor) => new Promise((resolve, reject) => {
    const bridge = window.electronBridge;
    if (typeof bridge?.sendMessageFromView !== "function") {
      reject(new Error("Codex Desktop model bridge is unavailable"));
      return;
    }
    const id = `little-mungbean-model-check-${Date.now()}-${Math.random()}`;
    const transfers = new Map();
    let settled = false;
    let timer;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      window.removeEventListener("message", receive, true);
      transfers.clear();
      if (error) reject(error);
      else resolve(value);
    };
    const consume = (transfer, tokens) => {
      const save = (value) => {
        const parent = transfer.stack.at(-1);
        if (!parent) transfer.root = value;
        else if (Array.isArray(parent.value)) parent.value.push(value);
        else {
          if (parent.key == null) throw new Error("Invalid model response object");
          Object.defineProperty(parent.value, parent.key, { value, enumerable: true, writable: true, configurable: true });
          parent.key = null;
        }
      };
      for (const token of tokens) {
        switch (token.type) {
          case "object-start":
          case "array-start": {
            const value = token.type === "array-start" ? [] : {};
            save(value);
            transfer.stack.push({ value, key: null });
            break;
          }
          case "container-end":
            if (!transfer.stack.pop()) throw new Error("Invalid model response container");
            break;
          case "key": transfer.stack.at(-1).key = token.value; break;
          case "value": save(token.value); break;
          case "string-start": transfer.string = []; transfer.target = token.target; break;
          case "string-chunk": transfer.string.push(token.value); break;
          case "string-end": {
            const value = transfer.string.join("");
            if (transfer.target === "key") transfer.stack.at(-1).key = value;
            else save(value);
            transfer.string = null;
            break;
          }
          default: throw new Error("Unsupported model response token");
        }
      }
    };
    const receive = (event) => {
      if (event.source != null && event.source !== window) return;
      let data = event.data;
      try {
        if (data?.marker === "codex-host-chunked-message-v1") {
          if (data.kind === "start") {
            transfers.set(data.transferId, { sequence: data.sequence + 1, stack: [], root: null, string: null, size: 0 });
            return;
          }
          const transfer = transfers.get(data.transferId);
          if (!transfer) return;
          if (transfer.sequence++ !== data.sequence) {
            transfers.delete(data.transferId);
            return;
          }
          if (data.kind === "chunk") {
            transfer.size += JSON.stringify(data.tokens).length;
            if (transfer.size > 32 * 1024 * 1024) {
              transfers.delete(data.transferId);
              return;
            }
            consume(transfer, data.tokens);
            return;
          }
          if (data.kind !== "end") return;
          transfers.delete(data.transferId);
          if (transfer.stack.length || transfer.string) return;
          data = transfer.root;
        }
        if (data?.type !== "mcp-response") return;
        const message = data.message || data.response;
        if (String(message?.id) !== id) return;
        if (event.data?.type === "mcp-response") event.stopImmediatePropagation?.();
        if (message.error) finish(new Error(message.error.message || "Model list request failed"));
        else if (!Array.isArray(message.result?.data)) finish(new Error("Invalid Codex model list"));
        else finish(null, message.result);
      } catch (error) {
        // Malformed unrelated host messages must never interrupt the application.
        transfers.clear();
      }
    };
    window.addEventListener("message", receive, true);
    timer = setTimeout(() => finish(new Error("Codex model list verification timed out")), 8000);
    try {
      Promise.resolve(bridge.sendMessageFromView({
        type: "mcp-request", hostId: "local",
        request: { id, method: "model/list", params: { cursor, limit: 100, includeHidden: false } },
      })).catch((error) => finish(error));
    } catch (error) { finish(error); }
  });

  const verifyModelList = () => {
    if (state.verificationPending) return state.verificationPending;
    state.verificationPending = (async () => {
      const names = [];
      const cursors = new Set();
      let cursor = null;
      do {
        const result = await readModelPage(cursor);
        for (const model of result.data) if (!model.hidden) names.push(model.model);
        cursor = result.nextCursor ?? null;
        if (cursor != null && cursors.has(cursor)) throw new Error("Model catalog pagination repeated a cursor");
        cursors.add(cursor);
        if (cursors.size > 100) throw new Error("Model catalog has too many pages");
      } while (cursor != null);
      const expected = new Set(state.names);
      if (names.length !== expected.size || new Set(names).size !== expected.size || names.some((name) => !expected.has(name))) {
        throw new Error(`Codex model list does not match the gateway catalog (${names.length}/${expected.size})`);
      }
      state.verifiedModels = names;
      state.verificationError = null;
      state.verified = true;
      return true;
    })().catch((error) => {
      state.verified = false;
      state.verificationError = String(error?.message || error);
      logFailure("native-model-list", error);
      return false;
    }).finally(() => { state.verificationPending = null; });
    return state.verificationPending;
  };

  const refreshPass = () => {
    try { patchModelPickerLabels(); } catch (error) { logFailure("model-labels", error); }
  };
  const configure = (config) => {
    state.config = { ...state.config, ...(config || {}) };
    state.verified = false;
    loadCatalog(true).then(() => {
      refreshPass();
      return verifyModelList();
    }).catch((error) => logFailure("catalog", error));
  };

  globalThis.__LITTLE_MUNGBEAN_MODEL_UNLOCK__ = {
    version: incoming.version || "unknown",
    strategy: "native-catalog-v1",
    configure,
    refresh: () => loadCatalog(true).then(verifyModelList),
    health: () => ({
      installed: true,
      catalogReady: state.loadedAt > 0 && (state.names.length > 0 || Boolean(state.config.allowEmptyModelList)),
      modelListVerified: state.verified === true,
      modelCount: state.names.length,
      verifiedModelCount: state.verifiedModels.length,
      catalogSource: state.catalogSource,
      pickerInteractionHookInstalled: state.pickerInteractionHookInstalled,
      verificationError: state.verificationError,
      failures: state.failures.slice(-5),
    }),
    _test: { modelNameForLabel, patchModelPickerLabels, isModelPickerInteraction, schedulePickerLabelRefresh, readModelPage },
  };
  installPickerInteractionPatch();
  configure(incoming);
  setInterval(refreshPass, 2000);
})();
