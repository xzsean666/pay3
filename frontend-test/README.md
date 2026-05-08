# Pay3 前端测试台

这是一个只用于 development/staging dry-run 的静态测试页面，部署目标是 Cloudflare Wrangler Workers Assets。

## 本地预览

```bash
cd frontend-test
npm run dev
```

默认地址是 `http://127.0.0.1:8788`。默认代理到 `http://127.0.0.1:3000` 的 Pay3 API。

## 部署

```bash
cd frontend-test
npm run deploy
```

部署到 Cloudflare 后，`PAY3_API_BASE_URL` 不能继续用 `127.0.0.1`，需要改成 Cloudflare 能访问到的 staging Pay3 API URL。

## 直连开关

默认调用模式是 Worker 代理：

- 页面调用同源 `/api/pay3/*`。
- Worker 注入 `PAY3_TEST_JWT`。
- 浏览器不需要跨域访问 Pay3 API。

本测试台也保留浏览器直连开关：

- `PAY3_DIRECT_ENABLED=true` 时页面允许切换到直连。
- `PAY3_EXPOSE_TEST_JWT=true` 时页面会拿到内置测试 token。
- 直连要求 Pay3 API 自己配置 CORS，否则浏览器会拦截请求。

正常测试部署如果要关闭直连，把 `wrangler.toml` 改为：

```toml
PAY3_DIRECT_ENABLED = "false"
PAY3_EXPOSE_TEST_JWT = "false"
```

## 测试 token

当前 `wrangler.toml` 内置的 token 使用 `.env.example` 的 development JWT 配置签发：

- `JWT_ISSUER=pay3`
- `JWT_AUDIENCE=pay3-api`
- `JWT_KEY_ID=pay3-dev-key-1`
- `JWT_SECRET=dev-dry-run-jwt-secret-000000000000`
- `scope=orders:create orders:read orders:verify`

如果后端 JWT 配置变了，可以重新生成：

```bash
cd frontend-test
npm run token
```

生产或长期公开环境不要把 `PAY3_TEST_JWT` 放在 `wrangler.toml`；改用 `wrangler secret put PAY3_TEST_JWT`，并关闭 `PAY3_EXPOSE_TEST_JWT`。
