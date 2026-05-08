#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const frontendRoot = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(frontendRoot, "..");

const args = parseArgs(process.argv.slice(2));
if (args.help) {
  printHelp();
  process.exit(0);
}

const envFile = path.resolve(process.cwd(), args.envFile || path.join(repoRoot, ".env.test"));
const fileEnv = readDotenv(envFile);

const dryRun = Boolean(args.dryRun);
const apiBaseUrl = resolveApiBaseUrl(fileEnv, args, dryRun);
const ttlSeconds = positiveInt(args.ttlSeconds || fileEnv.PAY3_FRONTEND_JWT_TTL_SECONDS, 7 * 24 * 60 * 60);
const now = Math.floor(Date.now() / 1000);
const expiresAt = new Date((now + ttlSeconds) * 1000).toISOString();
const directEnabled = args.directEnabled ?? boolValue(fileEnv.PAY3_FRONTEND_DIRECT_ENABLED, true);
const exposeToken = args.exposeToken ?? boolValue(fileEnv.PAY3_FRONTEND_EXPOSE_TEST_JWT, directEnabled);
const defaultMode = args.defaultMode || fileEnv.PAY3_FRONTEND_DEFAULT_MODE || "proxy";
const scopes =
  args.scopes ||
  fileEnv.PAY3_FRONTEND_TEST_SCOPES ||
  fileEnv.JWT_SCOPES ||
  "orders:create orders:read orders:verify";

validateDefaultMode(defaultMode);
validateApiBaseUrl(apiBaseUrl, { allowLocalApi: Boolean(args.allowLocalApi), dryRun });

const token = signHs256Jwt({
  secret: required(fileEnv, "JWT_SECRET", envFile),
  kid: required(fileEnv, "JWT_KEY_ID", envFile),
  issuer: required(fileEnv, "JWT_ISSUER", envFile),
  audience: required(fileEnv, "JWT_AUDIENCE", envFile),
  subject: args.subject || fileEnv.PAY3_FRONTEND_TEST_SUBJECT || "frontend-test",
  scopes,
  now,
  ttlSeconds,
});

const wranglerArgs = [
  "dlx",
  "wrangler@4.90.0",
  "deploy",
  "--var",
  `PAY3_API_BASE_URL:${apiBaseUrl}`,
  "--var",
  `PAY3_TEST_JWT:${token}`,
  "--var",
  `PAY3_TEST_JWT_EXPIRES_AT:${expiresAt}`,
  "--var",
  `PAY3_DEFAULT_MODE:${defaultMode}`,
  "--var",
  "PAY3_PROXY_ENABLED:true",
  "--var",
  `PAY3_DIRECT_ENABLED:${directEnabled ? "true" : "false"}`,
  "--var",
  `PAY3_EXPOSE_TEST_JWT:${exposeToken ? "true" : "false"}`,
];

if (dryRun) {
  wranglerArgs.push("--dry-run");
}
if (args.keepVars) {
  wranglerArgs.push("--keep-vars");
}

console.log(`Deploying Pay3 frontend test page with ${path.relative(process.cwd(), envFile) || envFile}`);
console.log(`Pay3 API: ${apiBaseUrl}`);
console.log(`JWT: HS256 kid=${fileEnv.JWT_KEY_ID} sub=${args.subject || fileEnv.PAY3_FRONTEND_TEST_SUBJECT || "frontend-test"} scopes="${scopes}" exp=${expiresAt}`);
console.log(`Mode: default=${defaultMode}, direct=${directEnabled ? "enabled" : "disabled"}, exposedToken=${exposeToken ? "yes" : "no"}`);

const result = spawnSync("pnpm", wranglerArgs, {
  cwd: frontendRoot,
  stdio: "inherit",
  env: process.env,
});

process.exit(result.status ?? 1);

function signHs256Jwt({ secret, kid, issuer, audience, subject, scopes, now, ttlSeconds }) {
  const header = { alg: "HS256", typ: "JWT", kid };
  const payload = {
    exp: now + ttlSeconds,
    nbf: now - 60,
    iat: now,
    iss: issuer,
    aud: audience,
    sub: subject,
    scope: scopes,
  };
  const signingInput = `${base64urlJson(header)}.${base64urlJson(payload)}`;
  const signature = crypto.createHmac("sha256", secret).update(signingInput).digest("base64url");
  return `${signingInput}.${signature}`;
}

function readDotenv(file) {
  if (!fs.existsSync(file)) {
    fail(`env file not found: ${file}`);
  }

  const env = {};
  const text = fs.readFileSync(file, "utf8");
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) {
      continue;
    }
    const index = trimmed.indexOf("=");
    if (index < 0) {
      continue;
    }
    const key = trimmed.slice(0, index).trim();
    let value = trimmed.slice(index + 1).trim();
    if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1);
    }
    env[key] = value;
  }
  return env;
}

function resolveApiBaseUrl(env, args, dryRun) {
  const configured =
    args.apiBaseUrl ||
    env.PAY3_FRONTEND_API_BASE_URL ||
    env.PAY3_API_BASE_URL ||
    process.env.PAY3_FRONTEND_API_BASE_URL ||
    process.env.PAY3_API_BASE_URL;

  if (configured) {
    return configured.replace(/\/+$/, "");
  }

  if (dryRun) {
    return "http://127.0.0.1:3000";
  }

  fail("missing Pay3 API URL. Pass --api-base-url https://your-pay3-api.example");
}

function validateApiBaseUrl(value, { allowLocalApi, dryRun }) {
  let url;
  try {
    url = new URL(value);
  } catch {
    fail(`invalid Pay3 API URL: ${value}`);
  }
  if (!["http:", "https:"].includes(url.protocol)) {
    fail("Pay3 API URL must use http or https");
  }
  const isLocal = ["127.0.0.1", "localhost", "::1"].includes(url.hostname);
  if (isLocal && !dryRun && !allowLocalApi) {
    fail("refusing to deploy with a local Pay3 API URL. Pass --allow-local-api only for local tunnel tests.");
  }
}

function validateDefaultMode(value) {
  if (!["proxy", "direct"].includes(value)) {
    fail("--default-mode must be proxy or direct");
  }
}

function required(env, key, file) {
  const value = env[key];
  if (!value) {
    fail(`${key} is required in ${file}`);
  }
  return value;
}

function positiveInt(value, fallback) {
  const parsed = Number(value || fallback);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    fail(`expected a positive integer, got ${value}`);
  }
  return parsed;
}

function boolValue(value, fallback) {
  if (value === undefined || value === null || value === "") {
    return fallback;
  }
  return ["1", "true", "yes", "on"].includes(String(value).toLowerCase());
}

function base64urlJson(value) {
  return Buffer.from(JSON.stringify(value)).toString("base64url");
}

function parseArgs(argv) {
  const parsed = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--") continue;
    if (arg === "--help" || arg === "-h") parsed.help = true;
    else if (arg === "--dry-run") parsed.dryRun = true;
    else if (arg === "--keep-vars") parsed.keepVars = true;
    else if (arg === "--direct") parsed.directEnabled = true;
    else if (arg === "--no-direct") parsed.directEnabled = false;
    else if (arg === "--expose-token") parsed.exposeToken = true;
    else if (arg === "--hide-token") parsed.exposeToken = false;
    else if (arg === "--allow-local-api") parsed.allowLocalApi = true;
    else if (arg === "--env-file") parsed.envFile = nextValue(argv, ++i, arg);
    else if (arg === "--api-base-url") parsed.apiBaseUrl = nextValue(argv, ++i, arg);
    else if (arg === "--ttl-seconds") parsed.ttlSeconds = nextValue(argv, ++i, arg);
    else if (arg === "--subject") parsed.subject = nextValue(argv, ++i, arg);
    else if (arg === "--scopes") parsed.scopes = nextValue(argv, ++i, arg);
    else if (arg === "--default-mode") parsed.defaultMode = nextValue(argv, ++i, arg);
    else fail(`unknown argument: ${arg}`);
  }
  return parsed;
}

function nextValue(argv, index, flag) {
  const value = argv[index];
  if (!value || value.startsWith("--")) {
    fail(`${flag} requires a value`);
  }
  return value;
}

function printHelp() {
  console.log(`Usage:
  pnpm run deploy:page -- --api-base-url https://pay3-api.example

Options:
  --env-file <path>       Env file with JWT_SECRET/JWT_ISSUER/JWT_AUDIENCE/JWT_KEY_ID. Default: ../.env.test
  --api-base-url <url>    Public/staging Pay3 API URL for the Worker proxy.
  --dry-run               Run wrangler deploy --dry-run.
  --ttl-seconds <n>       Test JWT TTL. Default: 604800.
  --subject <sub>         JWT sub. Default: frontend-test.
  --scopes <scopes>       JWT scopes. Default: orders:create orders:read orders:verify.
  --no-direct             Disable the browser-direct switch.
  --hide-token            Do not expose the generated JWT to the browser.
  --default-mode <mode>   proxy or direct. Default: proxy.
  --allow-local-api       Allow 127.0.0.1/localhost API URL on non-dry-run deploys.
  --keep-vars             Pass --keep-vars to wrangler deploy.
`);
}

function fail(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}
