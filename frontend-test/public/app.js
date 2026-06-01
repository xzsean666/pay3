const CONFIG_ENDPOINT = "/api/config";
const STORE_KEY = "pay3.frontend-test.v1";

const state = {
  config: {
    apiBaseUrl: "",
    proxyPath: "/api/pay3",
    defaultMode: "proxy",
    proxyEnabled: true,
    directEnabled: false,
    exposeTestJwt: false,
    testJwt: "",
    testJwtExpiresAt: "",
  },
  useProxy: true,
  qrMode: "uri",
  token: "",
  order: null,
  verifyResult: null,
  logs: [],
  pollTimer: null,
  lastPollAt: null,
};

const els = {};

document.addEventListener("DOMContentLoaded", init);

async function init() {
  collectEls();
  bindEvents();
  seedCreateForm();
  await loadRuntimeConfig();
  loadPrefs();
  applyConfigToControls();
  renderAll();
  setTab("config");
  setInterval(renderCountdown, 1000);
}

function collectEls() {
  for (const id of [
    "modeBadge",
    "connectionBadge",
    "proxyMode",
    "directMode",
    "directHint",
    "apiBaseUrl",
    "successRedirectUrl",
    "pollIntervalMs",
    "saveConfigBtn",
    "fillTokenBtn",
    "copyTokenBtn",
    "tokenField",
    "tokenMeta",
    "createOrderForm",
    "newOrderBtn",
    "externalId",
    "amount",
    "ttlSeconds",
    "metadata",
    "createOrderBtn",
    "recoverByExternalIdBtn",
    "emptyPayState",
    "paymentView",
    "qrUriBtn",
    "qrAddressBtn",
    "qrImage",
    "qrPayload",
    "paymentDetails",
    "copyAddressBtn",
    "copyAmountBtn",
    "copyPaymentUriBtn",
    "pollOnceBtn",
    "startPollBtn",
    "stopPollBtn",
    "orderStatusText",
    "paidRawText",
    "expiresInText",
    "lastPollText",
    "autoVerify",
    "verifyOnceBtn",
    "verifyResult",
    "successEmpty",
    "successView",
    "successDetails",
    "createAnotherBtn",
    "openRedirectBtn",
    "clearLogsBtn",
    "downloadLogsBtn",
    "logList",
    "toast",
  ]) {
    els[id] = document.getElementById(id);
  }
}

function bindEvents() {
  document.querySelectorAll("[data-tab]").forEach((button) => {
    button.addEventListener("click", () => setTab(button.dataset.tab));
  });

  els.proxyMode.addEventListener("change", () => setCallMode("proxy"));
  els.directMode.addEventListener("change", () => setCallMode("direct"));
  els.saveConfigBtn.addEventListener("click", () => {
    pullControlsToState();
    savePrefs();
    renderAll();
    toast("设置已保存");
  });

  els.fillTokenBtn.addEventListener("click", () => {
    els.tokenField.value = state.config.testJwt || "";
    pullControlsToState();
    savePrefs();
    toast(state.config.testJwt ? "已填入内置测试 token" : "当前配置没有暴露内置 token");
  });
  els.copyTokenBtn.addEventListener("click", () => copyText(els.tokenField.value, "token 已复制"));

  els.createOrderForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    await createOrder();
  });
  els.newOrderBtn.addEventListener("click", resetCreateForm);
  els.recoverByExternalIdBtn.addEventListener("click", recoverByExternalId);

  els.qrUriBtn.addEventListener("click", () => setQrMode("uri"));
  els.qrAddressBtn.addEventListener("click", () => setQrMode("address"));
  els.copyAddressBtn.addEventListener("click", () => copyText(state.order?.payment?.receive_address, "收款地址已复制"));
  els.copyAmountBtn.addEventListener("click", () => copyText(state.order?.payment?.amount, "金额已复制"));
  els.copyPaymentUriBtn.addEventListener("click", () => copyText(paymentUri(), "付款 URI 已复制"));

  els.pollOnceBtn.addEventListener("click", () => pollOrder({ verifyFirst: false }));
  els.startPollBtn.addEventListener("click", startPolling);
  els.stopPollBtn.addEventListener("click", () => {
    stopPolling();
    toast("轮询已停止");
  });
  els.verifyOnceBtn.addEventListener("click", verifyOnce);
  els.autoVerify.addEventListener("change", savePrefs);

  els.createAnotherBtn.addEventListener("click", () => {
    stopPolling();
    state.order = null;
    state.verifyResult = null;
    resetCreateForm();
    savePrefs();
    renderAll();
    setTab("create");
  });
  els.openRedirectBtn.addEventListener("click", openSuccessRedirect);

  els.clearLogsBtn.addEventListener("click", () => {
    state.logs = [];
    renderLogs();
  });
  els.downloadLogsBtn.addEventListener("click", downloadLogs);
}

async function loadRuntimeConfig() {
  try {
    const response = await fetch(CONFIG_ENDPOINT, { headers: { accept: "application/json" } });
    if (!response.ok) {
      throw new Error(`config endpoint returned ${response.status}`);
    }
    state.config = { ...state.config, ...(await response.json()) };
    log("config", "Loaded Wrangler runtime config", scrubConfig(state.config));
  } catch (error) {
    log("config", "Using static fallback config", errorMessage(error));
  }
}

function loadPrefs() {
  const raw = localStorage.getItem(STORE_KEY);
  if (!raw) {
    state.useProxy = state.config.defaultMode !== "direct";
    state.token = state.config.testJwt || "";
    return;
  }

  try {
    const prefs = JSON.parse(raw);
    state.useProxy = typeof prefs.useProxy === "boolean" ? prefs.useProxy : state.config.defaultMode !== "direct";
    state.token = prefs.token || state.config.testJwt || "";
    state.order = prefs.order || null;
    state.qrMode = prefs.qrMode === "address" ? "address" : "uri";
    els.apiBaseUrl.value = prefs.apiBaseUrl || state.config.apiBaseUrl || "";
    els.successRedirectUrl.value = prefs.successRedirectUrl || "";
    els.pollIntervalMs.value = String(prefs.pollIntervalMs || 3000);
    els.autoVerify.checked = Boolean(prefs.autoVerify);
  } catch (error) {
    log("config", "Failed to parse stored preferences", errorMessage(error));
    state.useProxy = state.config.defaultMode !== "direct";
    state.token = state.config.testJwt || "";
  }
}

function savePrefs() {
  const prefs = {
    useProxy: state.useProxy,
    token: els.tokenField.value.trim(),
    apiBaseUrl: els.apiBaseUrl.value.trim(),
    successRedirectUrl: els.successRedirectUrl.value.trim(),
    pollIntervalMs: Number(els.pollIntervalMs.value) || 3000,
    autoVerify: els.autoVerify.checked,
    order: state.order,
    qrMode: state.qrMode,
  };
  localStorage.setItem(STORE_KEY, JSON.stringify(prefs));
}

function applyConfigToControls() {
  if (!state.config.directEnabled && !state.useProxy) {
    state.useProxy = true;
  }
  if (!state.config.proxyEnabled && state.useProxy) {
    state.useProxy = false;
  }

  if (!els.apiBaseUrl.value) {
    els.apiBaseUrl.value = state.config.apiBaseUrl || "";
  }
  els.tokenField.value = state.token || state.config.testJwt || "";
  els.proxyMode.checked = state.useProxy;
  els.directMode.checked = !state.useProxy;
  els.directMode.disabled = !state.config.directEnabled;
  els.proxyMode.disabled = !state.config.proxyEnabled;
  els.directHint.textContent = state.config.directEnabled
    ? "直连开关已开启，但仍需要后端 CORS 白名单允许这个页面的域名。"
    : "当前 Wrangler 配置关闭了浏览器直连。";
  els.tokenMeta.textContent = tokenMetaText();
}

function pullControlsToState() {
  state.useProxy = els.proxyMode.checked;
  state.token = els.tokenField.value.trim();
}

function setCallMode(mode) {
  if (mode === "direct" && !state.config.directEnabled) {
    els.proxyMode.checked = true;
    toast("当前配置关闭了浏览器直连");
    return;
  }
  if (mode === "proxy" && !state.config.proxyEnabled) {
    els.directMode.checked = true;
    toast("当前配置关闭了 Worker 代理");
    return;
  }
  state.useProxy = mode !== "direct";
  savePrefs();
  renderTopStatus();
}

function seedCreateForm() {
  els.externalId.value = newExternalId();
  els.metadata.value = JSON.stringify({ source: "frontend-test", channel: "wrangler" }, null, 2);
}

function resetCreateForm() {
  seedCreateForm();
  els.amount.value = "1.00";
  els.ttlSeconds.value = "900";
}

function newExternalId() {
  const stamp = new Date().toISOString().replace(/[-:.TZ]/g, "").slice(0, 14);
  const random = Math.random().toString(16).slice(2, 8);
  return `frontend-test-${stamp}-${random}`;
}

async function createOrder() {
  pullControlsToState();
  const metadata = parseMetadata();
  if (!metadata.ok) {
    toast(metadata.message);
    return;
  }

  const body = {
    external_id: els.externalId.value.trim(),
    amount: els.amount.value.trim(),
    ttl_seconds: Number(els.ttlSeconds.value),
    metadata: metadata.value,
  };

  if (!body.external_id || !body.amount || !body.ttl_seconds) {
    toast("订单字段不完整");
    return;
  }

  setBusy(els.createOrderBtn, true, "创建中");
  try {
    const order = await apiRequest("/v1/orders", { method: "POST", body });
    state.order = order;
    state.verifyResult = null;
    savePrefs();
    renderAll();
    setTab("pay");
    startPolling();
    toast("订单已创建");
  } catch (error) {
    showError(error);
  } finally {
    setBusy(els.createOrderBtn, false, "创建订单");
  }
}

async function recoverByExternalId() {
  const externalId = els.externalId.value.trim();
  if (!externalId) {
    toast("先填写 external_id");
    return;
  }
  try {
    state.order = await apiRequest(`/v1/orders/by-external-id/${encodeURIComponent(externalId)}`);
    savePrefs();
    renderAll();
    setTab("pay");
    startPolling();
    toast("订单已恢复");
  } catch (error) {
    showError(error);
  }
}

async function pollOrder({ verifyFirst = els.autoVerify.checked } = {}) {
  if (!state.order?.id) {
    toast("还没有订单可查询");
    return;
  }

  try {
    if (verifyFirst) {
      try {
        await verifyOnce({ quiet: true });
      } catch (error) {
        log("verify", "Auto verify failed", serializeError(error));
      }
    }

    state.order = await apiRequest(`/v1/orders/${state.order.id}`);
    state.lastPollAt = new Date();
    savePrefs();
    renderAll();

    if (state.order.status === "paid") {
      handlePaid();
    }
  } catch (error) {
    showError(error);
  }
}

async function verifyOnce({ quiet = false } = {}) {
  if (!state.order?.id) {
    toast("还没有订单可 verify");
    return;
  }

  const result = await apiRequest(`/v1/orders/${state.order.id}/verify`, { method: "POST" });
  state.verifyResult = result;
  els.verifyResult.textContent = JSON.stringify(result, null, 2);
  if (!quiet) {
    toast("verify 已完成");
  }
  return result;
}

function startPolling() {
  if (!state.order?.id) {
    toast("还没有订单可轮询");
    return;
  }
  stopPolling();
  const interval = Math.max(1000, Number(els.pollIntervalMs.value) || 3000);
  state.pollTimer = setInterval(() => pollOrder(), interval);
  pollOrder();
  renderTopStatus();
}

function stopPolling() {
  if (state.pollTimer) {
    clearInterval(state.pollTimer);
    state.pollTimer = null;
  }
  renderTopStatus();
}

function handlePaid() {
  stopPolling();
  renderAll();
  const redirectUrl = successRedirectUrl();
  if (redirectUrl) {
    log("success", "Order paid; redirect URL prepared", redirectUrl);
  }
  setTab("success");
  toast("支付成功");
}

function setQrMode(mode) {
  state.qrMode = mode === "address" ? "address" : "uri";
  savePrefs();
  renderPayment();
}

function renderAll() {
  renderTopStatus();
  renderPayment();
  renderWait();
  renderSuccess();
  renderLogs();
}

function renderTopStatus() {
  els.modeBadge.textContent = state.useProxy ? "代理模式" : "浏览器直连";
  els.modeBadge.className = state.useProxy ? "badge" : "badge warn";
  const statusParts = [];
  if (state.order?.id) {
    statusParts.push(statusLabel(state.order.status));
  }
  if (state.pollTimer) {
    statusParts.push("轮询中");
  }
  els.connectionBadge.textContent = statusParts.join(" / ") || "未连接";
  els.connectionBadge.className = state.order?.status === "paid" ? "badge ok" : "badge muted";
}

function renderPayment() {
  const hasOrder = Boolean(state.order?.payment);
  els.emptyPayState.classList.toggle("hidden", hasOrder);
  els.paymentView.classList.toggle("hidden", !hasOrder);
  els.qrUriBtn.classList.toggle("active", state.qrMode === "uri");
  els.qrAddressBtn.classList.toggle("active", state.qrMode === "address");

  if (!hasOrder) {
    return;
  }

  const qrData = state.qrMode === "address" ? state.order.payment.receive_address : paymentUri();
  els.qrPayload.value = qrData;
  els.qrImage.src = `https://api.qrserver.com/v1/create-qr-code/?size=300x300&margin=10&format=svg&data=${encodeURIComponent(qrData)}`;
  els.paymentDetails.innerHTML = detailRows([
    ["订单 ID", state.order.id],
    ["external_id", state.order.external_id],
    ["状态", statusLabel(state.order.status)],
    ["链 ID", state.order.payment.chain_id],
    ["Token", `${state.order.payment.token_symbol} (${state.order.payment.token_address})`],
    ["金额", `${state.order.payment.amount} ${state.order.payment.token_symbol}`],
    ["amount_raw", state.order.payment.amount_raw],
    ["paid_amount_raw", state.order.payment.paid_amount_raw],
    ["收款地址", state.order.payment.receive_address],
    ["过期时间", formatDate(state.order.payment.expires_at)],
    ["监控到", formatDate(state.order.payment.monitor_until)],
  ]);
}

function renderWait() {
  const order = state.order;
  els.orderStatusText.textContent = order ? statusLabel(order.status) : "未创建";
  els.paidRawText.textContent = order?.payment?.paid_amount_raw || "0";
  els.lastPollText.textContent = state.lastPollAt ? state.lastPollAt.toLocaleTimeString() : "-";
  els.verifyResult.textContent = state.verifyResult ? JSON.stringify(state.verifyResult, null, 2) : "";
  renderCountdown();
}

function renderSuccess() {
  const paid = state.order?.status === "paid";
  els.successEmpty.classList.toggle("hidden", paid);
  els.successView.classList.toggle("hidden", !paid);
  if (!paid) {
    return;
  }

  els.successDetails.innerHTML = detailRows([
    ["订单 ID", state.order.id],
    ["external_id", state.order.external_id],
    ["状态", statusLabel(state.order.status)],
    ["金额", `${state.order.payment.amount} ${state.order.payment.token_symbol}`],
    ["paid_amount_raw", state.order.payment.paid_amount_raw],
    ["收款地址", state.order.payment.receive_address],
    ["跳转 URL", successRedirectUrl() || "内置成功页"],
  ]);
  els.openRedirectBtn.disabled = !successRedirectUrl();
}

function renderCountdown() {
  if (!state.order?.payment?.expires_at) {
    els.expiresInText.textContent = "-";
    return;
  }
  const expiresAt = new Date(state.order.payment.expires_at).getTime();
  const remaining = expiresAt - Date.now();
  els.expiresInText.textContent = remaining <= 0 ? "已过期" : formatDuration(remaining);
}

function renderLogs() {
  els.logList.innerHTML = state.logs
    .slice()
    .reverse()
    .map(
      (entry) => `
        <li>
          <time>${escapeHtml(entry.time)}</time>
          <strong>${escapeHtml(entry.type)}</strong>
          <div>${escapeHtml(entry.message)}</div>
          ${entry.data === undefined ? "" : `<pre>${escapeHtml(JSON.stringify(entry.data, null, 2))}</pre>`}
        </li>
      `,
    )
    .join("");
}

async function apiRequest(path, { method = "GET", body } = {}) {
  pullControlsToState();
  savePrefs();

  const requestId = `pay3-test-${Date.now()}-${Math.random().toString(16).slice(2, 8)}`;
  const headers = {
    accept: "application/json",
    "x-request-id": requestId,
  };

  let url;
  if (state.useProxy) {
    url = joinPath(state.config.proxyPath || "/api/pay3", path);
  } else {
    const baseUrl = els.apiBaseUrl.value.trim().replace(/\/+$/, "");
    if (!baseUrl) {
      throw new Error("浏览器直连需要填写 Pay3 API Base URL");
    }
    if (!state.token) {
      throw new Error("浏览器直连需要测试 token");
    }
    url = `${baseUrl}${path}`;
    headers.authorization = `Bearer ${state.token}`;
  }

  const init = { method, headers };
  if (body !== undefined) {
    headers["content-type"] = "application/json";
    init.body = JSON.stringify(body);
  }

  const started = performance.now();
  try {
    const response = await fetch(url, init);
    const data = await readResponse(response);
    log("http", `${method} ${path} -> ${response.status}`, {
      request_id: requestId,
      duration_ms: Math.round(performance.now() - started),
      response: data,
    });

    if (!response.ok) {
      const error = new Error(errorMessageFromPayload(data, response.status));
      error.status = response.status;
      error.payload = data;
      throw error;
    }

    return data;
  } catch (error) {
    if (error instanceof TypeError && !state.useProxy) {
      error.message = `${error.message}. 浏览器直连失败时优先检查 Pay3 API CORS。`;
    }
    log("error", `${method} ${path} failed`, serializeError(error));
    throw error;
  }
}

async function readResponse(response) {
  const contentType = response.headers.get("content-type") || "";
  if (contentType.includes("application/json")) {
    return response.json();
  }
  return response.text();
}

function parseMetadata() {
  const raw = els.metadata.value.trim();
  if (!raw) {
    return { ok: true, value: {} };
  }
  try {
    const parsed = JSON.parse(raw);
    if (parsed === null || Array.isArray(parsed) || typeof parsed !== "object") {
      return { ok: false, message: "metadata 必须是 JSON object" };
    }
    return { ok: true, value: parsed };
  } catch (error) {
    return { ok: false, message: `metadata JSON 无效：${error.message}` };
  }
}

function paymentUri() {
  if (!state.order?.payment) {
    return "";
  }
  const payment = state.order.payment;
  return `ethereum:${payment.token_address}@${payment.chain_id}/transfer?address=${payment.receive_address}&uint256=${payment.amount_raw}`;
}

function successRedirectUrl() {
  const raw = els.successRedirectUrl.value.trim();
  if (!raw || !state.order) {
    return "";
  }
  try {
    const url = new URL(raw);
    url.searchParams.set("order_id", state.order.id);
    url.searchParams.set("external_id", state.order.external_id);
    url.searchParams.set("status", state.order.status);
    return url.toString();
  } catch {
    return "";
  }
}

function openSuccessRedirect() {
  const url = successRedirectUrl();
  if (!url) {
    toast("没有配置有效的成功跳转 URL");
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

function detailRows(rows) {
  return rows
    .map(([key, value]) => `<dt>${escapeHtml(String(key))}</dt><dd>${escapeHtml(String(value ?? ""))}</dd>`)
    .join("");
}

function setTab(tab) {
  document.querySelectorAll("[data-tab]").forEach((button) => {
    button.classList.toggle("active", button.dataset.tab === tab);
  });
  document.querySelectorAll("[data-panel]").forEach((panel) => {
    panel.classList.toggle("active", panel.dataset.panel === tab);
  });
}

function setBusy(button, busy, label) {
  button.disabled = busy;
  button.textContent = busy ? label : button.dataset.originalLabel || label;
}

function log(type, message, data) {
  state.logs.push({
    time: new Date().toISOString(),
    type,
    message,
    data,
  });
  if (state.logs.length > 120) {
    state.logs.splice(0, state.logs.length - 120);
  }
  if (els.logList) {
    renderLogs();
  }
}

function downloadLogs() {
  const blob = new Blob([JSON.stringify(state.logs, null, 2)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = `pay3-frontend-test-${Date.now()}.json`;
  a.click();
  URL.revokeObjectURL(url);
}

async function copyText(value, message) {
  if (!value) {
    toast("没有可复制的内容");
    return;
  }
  await navigator.clipboard.writeText(value);
  toast(message);
}

function showError(error) {
  toast(errorMessage(error));
}

function toast(message) {
  els.toast.textContent = message;
  els.toast.classList.add("show");
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => els.toast.classList.remove("show"), 2600);
}

function tokenMetaText() {
  const exposed = state.config.exposeTestJwt ? "内置 token 已暴露给页面" : "内置 token 未暴露给页面";
  return `${exposed}。长期测试 token。`;
}

function statusLabel(status) {
  const labels = {
    pending: "待支付",
    partial: "部分支付",
    confirming: "确认中",
    paid: "已支付",
    expired: "已过期",
  };
  return labels[status] || status || "未知";
}

function formatDate(value) {
  if (!value) {
    return "-";
  }
  return new Date(value).toLocaleString();
}

function formatDuration(ms) {
  const total = Math.ceil(ms / 1000);
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  return `${minutes}分${String(seconds).padStart(2, "0")}秒`;
}

function joinPath(prefix, path) {
  return `${prefix.replace(/\/+$/, "")}/${path.replace(/^\/+/, "")}`;
}

function errorMessage(error) {
  if (typeof error === "string") {
    return error;
  }
  if (error?.payload) {
    return errorMessageFromPayload(error.payload, error.status);
  }
  return error?.message || String(error);
}

function errorMessageFromPayload(payload, status) {
  if (payload?.error?.message) {
    return `${payload.error.code || status}: ${payload.error.message}`;
  }
  if (typeof payload === "string") {
    return payload;
  }
  return `HTTP ${status}`;
}

function serializeError(error) {
  return {
    message: errorMessage(error),
    status: error?.status,
    payload: error?.payload,
  };
}

function scrubConfig(config) {
  return {
    ...config,
    testJwt: config.testJwt ? "<exposed>" : "",
  };
}

function escapeHtml(value) {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}
