import crypto from "node:crypto";

const secret = env("JWT_SECRET", "dev-dry-run-jwt-secret-000000000000");
const kid = env("JWT_KEY_ID", "pay3-dev-key-1");
const issuer = env("JWT_ISSUER", "pay3");
const audience = env("JWT_AUDIENCE", "pay3-api");
const subject = env("JWT_SUBJECT", "frontend-test");
const scopes = env("JWT_SCOPES", "orders:create orders:read orders:verify");
const ttlSeconds = Number(env("JWT_TTL_SECONDS", String(60 * 60 * 24 * 365)));
const now = Math.floor(Date.now() / 1000);

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

console.log(`${signingInput}.${signature}`);

function base64urlJson(value) {
  return Buffer.from(JSON.stringify(value)).toString("base64url");
}

function env(name, fallback) {
  return process.env[name] || fallback;
}
