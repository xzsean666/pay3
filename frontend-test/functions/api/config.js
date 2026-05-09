import { jsonResponse, publicConfig } from "../_pay3.js";

export function onRequest({ env }) {
  return jsonResponse(publicConfig(env));
}
