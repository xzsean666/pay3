import { proxyPay3 } from "../_pay3.js";

export function onRequest({ request, env }) {
  return proxyPay3(request, env);
}
