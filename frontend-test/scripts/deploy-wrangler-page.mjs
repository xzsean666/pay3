#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const frontendRoot = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(frontendRoot, "..");
const DEFAULT_TEST_SCOPES = "orders:create orders:read orders:verify";

const args = parseArgs(process.argv.slice(2));
if (args.help) {
  printHelp();
  process.exit(0);
}

const envFile = path.resolve(process.cwd(), args.envFile || path.join(repoRoot, ".env.test"));
const fileEnv = readDotenv(envFile);

const dryRun = Boolean(args.dryRun);
const apiBaseUrl = resolveApiBaseUrl(args, dryRun);
const ttlSeconds = positiveInt(args.ttlSeconds || fileEnv.PAY3_FRONTEND_JWT_TTL_SECONDS, 7 * 24 * 60 * 60);
const now = Math.floor(Date.now() / 1000);
const directEnabled = args.directEnabled ?? boolValue(fileEnv.PAY3_FRONTEND_DIRECT_ENABLED, true);
const exposeToken = args.exposeToken ?? boolValue(fileEnv.PAY3_FRONTEND_EXPOSE_TEST_JWT, directEnabled);
const defaultMode = args.defaultMode || fileEnv.PAY3_FRONTEND_DEFAULT_MODE || "proxy";
const target = args.target || fileEnv.PAY3_FRONTEND_DEPLOY_TARGET || "pages";
const createPagesProject = args.createPagesProject ?? boolValue(fileEnv.PAY3_FRONTEND_CREATE_PAGES_PROJECT, true);
const projectName =
  args.projectName ||
  fileEnv.PAY3_FRONTEND_PAGES_PROJECT_NAME ||
  process.env.PAY3_FRONTEND_PAGES_PROJECT_NAME ||
  "pay3-frontend-test";
const scopes =
  args.scopes ||
  fileEnv.PAY3_FRONTEND_TEST_SCOPES ||
  fileEnv.JWT_SCOPES ||
  DEFAULT_TEST_SCOPES;

validateDefaultMode(defaultMode);
validateTarget(target);
validateApiBaseUrl(apiBaseUrl, { allowLocalApi: Boolean(args.allowLocalApi), dryRun });

const jwt = resolveTestJwt({
  fileEnv,
  envFile,
  args,
  now,
  ttlSeconds,
  scopes,
});

const deployVars = {
  PAY3_API_BASE_URL: apiBaseUrl,
  PAY3_TEST_JWT: jwt.token,
  PAY3_TEST_JWT_EXPIRES_AT: jwt.expiresAt,
  PAY3_DEFAULT_MODE: defaultMode,
  PAY3_PROXY_ENABLED: "true",
  PAY3_DIRECT_ENABLED: directEnabled ? "true" : "false",
  PAY3_EXPOSE_TEST_JWT: exposeToken ? "true" : "false",
};

console.log(`Deploying Pay3 frontend test page with ${path.relative(process.cwd(), envFile) || envFile}`);
console.log(`Pay3 API: ${apiBaseUrl}`);
console.log(`JWT: ${jwt.summary}`);
console.log(`Target: ${target}${target === "pages" ? ` project=${projectName}` : ""}`);
console.log(`Mode: default=${defaultMode}, direct=${directEnabled ? "enabled" : "disabled"}, exposedToken=${exposeToken ? "yes" : "no"}`);

if (target === "pages") {
  process.exit(deployPages({ projectName, branch: args.branch, deployVars, dryRun, createPagesProject }));
} else {
  process.exit(deployWorkerAssets({ deployVars, dryRun, keepVars: args.keepVars }));
}

function deployPages({ projectName, branch, deployVars, dryRun, createPagesProject }) {
  const productionBranch = branch || "main";
  if (dryRun) {
    if (createPagesProject) {
      console.log(`Dry run: would ensure Pages project exists: ${projectName} productionBranch=${productionBranch}`);
    }
    console.log("Dry run: would upload Pages secrets:");
    for (const key of Object.keys(deployVars)) {
      console.log(`  ${key}${key === "PAY3_TEST_JWT" ? "=<redacted>" : `=${deployVars[key]}`}`);
    }
    console.log(
      `Dry run: would run pnpm dlx wrangler@4.90.0 pages deploy public --project-name ${projectName}${
        branch ? ` --branch ${branch}` : ""
      } --commit-dirty=true`
    );
    return 0;
  }

  if (createPagesProject && !ensurePagesProject(projectName, productionBranch)) {
    return 1;
  }

  const tmpDir = fs.mkdtempSync(path.join(os.tmpdir(), "pay3-pages-vars-"));
  const varsFile = path.join(tmpDir, "vars.json");
  fs.writeFileSync(varsFile, JSON.stringify(deployVars, null, 2), { mode: 0o600 });

  try {
    const secretResult = spawnSync(
      "pnpm",
      ["dlx", "wrangler@4.90.0", "pages", "secret", "bulk", varsFile, "--project-name", projectName],
      { cwd: frontendRoot, stdio: "inherit", env: process.env }
    );
    if ((secretResult.status ?? 1) !== 0) {
      return secretResult.status ?? 1;
    }

    const deployArgs = [
      "dlx",
      "wrangler@4.90.0",
      "pages",
      "deploy",
      "public",
      "--project-name",
      projectName,
      "--commit-dirty=true",
    ];
    if (branch) {
      deployArgs.push("--branch", branch);
    }

    const deployResult = spawnSync("pnpm", deployArgs, {
      cwd: frontendRoot,
      stdio: "inherit",
      env: process.env,
    });
    return deployResult.status ?? 1;
  } finally {
    fs.rmSync(tmpDir, { recursive: true, force: true });
  }
}

function ensurePagesProject(projectName, productionBranch) {
  const listResult = spawnSync("pnpm", ["dlx", "wrangler@4.90.0", "pages", "project", "list"], {
    cwd: frontendRoot,
    stdio: ["ignore", "pipe", "pipe"],
    encoding: "utf8",
    env: process.env,
  });
  const listOutput = `${listResult.stdout || ""}\n${listResult.stderr || ""}`;
  if ((listResult.status ?? 1) === 0 && pagesProjectListIncludes(listOutput, projectName)) {
    console.log(`Pages project exists: ${projectName}`);
    return true;
  }

  if ((listResult.status ?? 1) !== 0) {
    process.stdout.write(listResult.stdout || "");
    process.stderr.write(listResult.stderr || "");
    console.error("warning: could not list Pages projects; trying to create the project");
  }

  console.log(`Creating Pages project: ${projectName}`);
  const createResult = spawnSync(
    "pnpm",
    [
      "dlx",
      "wrangler@4.90.0",
      "pages",
      "project",
      "create",
      projectName,
      "--production-branch",
      productionBranch,
    ],
    { cwd: frontendRoot, stdio: ["ignore", "pipe", "pipe"], encoding: "utf8", env: process.env }
  );
  const createOutput = `${createResult.stdout || ""}\n${createResult.stderr || ""}`;
  process.stdout.write(createResult.stdout || "");
  process.stderr.write(createResult.stderr || "");
  if ((createResult.status ?? 1) === 0) {
    return true;
  }
  if (/already exists|already owns a project|project.*exists/i.test(createOutput)) {
    console.log(`Pages project already exists: ${projectName}`);
    return true;
  }
  return false;
}

function pagesProjectListIncludes(output, projectName) {
  return output
    .split(/\r?\n/)
    .some((line) => line.split(/\s+/).includes(projectName));
}

function deployWorkerAssets({ deployVars, dryRun, keepVars }) {
  const wranglerArgs = ["dlx", "wrangler@4.90.0", "deploy"];
  for (const [key, value] of Object.entries(deployVars)) {
    wranglerArgs.push("--var", `${key}:${value}`);
  }
  if (dryRun) {
    wranglerArgs.push("--dry-run");
  }
  if (keepVars) {
    wranglerArgs.push("--keep-vars");
  }

  const result = spawnSync("pnpm", wranglerArgs, {
    cwd: frontendRoot,
    stdio: "inherit",
    env: process.env,
  });
  return result.status ?? 1;
}

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

function resolveTestJwt({ fileEnv, envFile, args, now, ttlSeconds, scopes }) {
  const providedJwt =
    fileEnv.PAY3_FRONTEND_TEST_JWT ||
    fileEnv.PAY3_API_JWT ||
    fileEnv.PAY3_TEST_JWT ||
    process.env.PAY3_FRONTEND_TEST_JWT ||
    process.env.PAY3_API_JWT ||
    process.env.PAY3_TEST_JWT;
  const subject = args.subject || fileEnv.PAY3_FRONTEND_TEST_SUBJECT || "frontend-test";
  const expiresAt = new Date((now + ttlSeconds) * 1000).toISOString();
  const kid = fileEnv.JWT_KEY_ID || fileEnv.JWT_KID;

  if (fileEnv.JWT_SECRET && kid && fileEnv.JWT_ISSUER && fileEnv.JWT_AUDIENCE) {
    return {
      token: signHs256Jwt({
        secret: fileEnv.JWT_SECRET,
        kid,
        issuer: fileEnv.JWT_ISSUER,
        audience: fileEnv.JWT_AUDIENCE,
        subject,
        scopes,
        now,
        ttlSeconds,
      }),
      expiresAt,
      summary: `generated HS256 kid=${kid} sub=${subject} scopes="${scopes}" exp=${expiresAt}`,
    };
  }

  if (providedJwt) {
    const providedExp = jwtExpSeconds(providedJwt);
    if (providedExp && providedExp <= now) {
      fail(`provided JWT is expired at ${new Date(providedExp * 1000).toISOString()}`);
    }
    const providedExpiresAt = providedExp ? new Date(providedExp * 1000).toISOString() : "";
    return {
      token: providedJwt,
      expiresAt: providedExpiresAt || "",
      summary: `from .env.test or shell env${providedExpiresAt ? ` exp=${providedExpiresAt}` : ""}`,
    };
  }

  fail(
    `JWT config is missing in ${envFile}. Set JWT_SECRET/JWT_KEY_ID/JWT_ISSUER/JWT_AUDIENCE or provide PAY3_API_JWT/PAY3_TEST_JWT.`
  );
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

function resolveApiBaseUrl(args, dryRun) {
  const configured =
    args.apiBaseUrl ||
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

function jwtExpSeconds(token) {
  const parts = String(token).split(".");
  if (parts.length < 2) {
    return 0;
  }

  try {
    const payload = JSON.parse(Buffer.from(parts[1], "base64url").toString("utf8"));
    if (!Number.isFinite(payload.exp)) {
      return 0;
    }
    return Number(payload.exp);
  } catch {
    return 0;
  }
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

function validateTarget(value) {
  if (!["pages", "worker"].includes(value)) {
    fail("--target must be pages or worker");
  }
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
    else if (arg === "--create-project") parsed.createPagesProject = true;
    else if (arg === "--no-create-project") parsed.createPagesProject = false;
    else if (arg === "--expose-token") parsed.exposeToken = true;
    else if (arg === "--hide-token") parsed.exposeToken = false;
    else if (arg === "--allow-local-api") parsed.allowLocalApi = true;
    else if (arg === "--env-file") parsed.envFile = nextValue(argv, ++i, arg);
    else if (arg === "--api-base-url") parsed.apiBaseUrl = nextValue(argv, ++i, arg);
    else if (arg === "--project-name") parsed.projectName = nextValue(argv, ++i, arg);
    else if (arg === "--branch") parsed.branch = nextValue(argv, ++i, arg);
    else if (arg === "--target") parsed.target = nextValue(argv, ++i, arg);
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
  pnpm run deploy:cloudflare -- --api-base-url https://pay3-api.example

Options:
  --env-file <path>       Env file with JWT_SECRET/JWT_ISSUER/JWT_AUDIENCE/JWT_KEY_ID, or PAY3_API_JWT/PAY3_TEST_JWT. Default: ../.env.test
  --api-base-url <url>    Public/staging Pay3 API URL for the Pages proxy.
  --project-name <name>   Cloudflare Pages project name. Default: pay3-frontend-test.
  --branch <name>         Pages deployment branch.
  --target <name>         pages or worker. Default: pages.
  --dry-run               Print the Pages deploy plan without uploading secrets or deploying.
  --no-create-project     Do not create the Cloudflare Pages project when missing.
  --ttl-seconds <n>       Test JWT TTL. Default: 604800.
  --subject <sub>         JWT sub. Default: frontend-test.
  --scopes <scopes>       JWT scopes. Default: orders:create orders:read orders:verify.
  --no-direct             Disable the browser-direct switch.
  --hide-token            Do not expose the generated JWT to the browser.
  --default-mode <mode>   proxy or direct. Default: proxy.
  --allow-local-api       Allow 127.0.0.1/localhost API URL on non-dry-run deploys.
  --keep-vars             Legacy worker-assets flag retained for compatibility.
`);
}

function fail(message) {
  console.error(`error: ${message}`);
  process.exit(1);
}
