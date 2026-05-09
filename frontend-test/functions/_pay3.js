const PROXY_PREFIX = "/api/pay3";

export function publicConfig(env) {
  const exposeToken = boolEnv(env.PAY3_EXPOSE_TEST_JWT, false);
  const directEnabled = boolEnv(env.PAY3_DIRECT_ENABLED, false);
  const proxyEnabled = boolEnv(env.PAY3_PROXY_ENABLED, true);
  const defaultMode = String(env.PAY3_DEFAULT_MODE || "proxy").toLowerCase() === "direct" ? "direct" : "proxy";

  return {
    apiBaseUrl: String(env.PAY3_API_BASE_URL || ""),
    proxyPath: PROXY_PREFIX,
    defaultMode,
    proxyEnabled,
    directEnabled,
    exposeTestJwt: exposeToken,
    testJwt: exposeToken ? String(env.PAY3_TEST_JWT || "") : "",
    testJwtExpiresAt: String(env.PAY3_TEST_JWT_EXPIRES_AT || ""),
  };
}

export async function proxyPay3(request, env) {
  if (!boolEnv(env.PAY3_PROXY_ENABLED, true)) {
    return jsonResponse({ error: { code: "proxy_disabled", message: "Pay3 proxy is disabled" } }, 403);
  }

  const token = String(env.PAY3_TEST_JWT || "").trim();
  if (!token) {
    return jsonResponse(
      { error: { code: "missing_test_jwt", message: "PAY3_TEST_JWT is not configured" } },
      500,
    );
  }

  const incomingUrl = new URL(request.url);
  const upstreamBase = String(env.PAY3_API_BASE_URL || "").trim().replace(/\/+$/, "");
  if (!upstreamBase) {
    return jsonResponse(
      { error: { code: "missing_api_base_url", message: "PAY3_API_BASE_URL is not configured" } },
      500,
    );
  }

  const upstreamPath = incomingUrl.pathname.slice(PROXY_PREFIX.length) || "/";
  let upstreamUrl;
  try {
    upstreamUrl = new URL(`${upstreamBase}${upstreamPath}${incomingUrl.search}`);
  } catch {
    return jsonResponse(
      { error: { code: "invalid_api_base_url", message: "PAY3_API_BASE_URL is not a valid URL" } },
      500,
    );
  }

  const headers = proxyHeaders(request.headers, token);
  try {
    const upstreamResponse = await fetch(upstreamUrl, {
      method: request.method,
      headers,
      body: request.method === "GET" || request.method === "HEAD" ? undefined : request.body,
      redirect: "manual",
    });

    const responseHeaders = new Headers(upstreamResponse.headers);
    responseHeaders.set("cache-control", "no-store");
    responseHeaders.set("x-pay3-test-proxy", "1");
    return new Response(upstreamResponse.body, {
      status: upstreamResponse.status,
      statusText: upstreamResponse.statusText,
      headers: responseHeaders,
    });
  } catch (error) {
    return jsonResponse(
      {
        error: {
          code: "upstream_unreachable",
          message: "Pay3 API upstream is unreachable",
          details: { upstream: upstreamBase, reason: error instanceof Error ? error.message : String(error) },
        },
      },
      502,
    );
  }
}

export function jsonResponse(body, status = 200) {
  return new Response(JSON.stringify(body, null, 2), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "no-store",
    },
  });
}

function proxyHeaders(inputHeaders, token) {
  const headers = new Headers(inputHeaders);
  const drop = [
    "authorization",
    "connection",
    "content-length",
    "host",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
  ];

  for (const key of drop) {
    headers.delete(key);
  }

  headers.set("authorization", `Bearer ${token}`);
  headers.set("accept", headers.get("accept") || "application/json");
  return headers;
}

function boolEnv(value, fallback) {
  if (value === undefined || value === null || value === "") {
    return fallback;
  }
  return ["1", "true", "yes", "on"].includes(String(value).toLowerCase());
}
