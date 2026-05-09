# Pay3 前端测试台

这是一个只用于 development/staging dry-run 的静态测试页面，部署目标是 Cloudflare Pages。

## 本地预览

```bash
cd frontend-test
pnpm run dev
```

默认地址是 `http://127.0.0.1:8788`。默认代理到 `http://127.0.0.1:3000` 的 Pay3 API。

## 部署

```bash
cd frontend-test
pnpm run deploy
```

部署到 Cloudflare 后，`PAY3_API_BASE_URL` 不能继续用 `127.0.0.1`，需要改成 Cloudflare 能访问到的 staging Pay3 API URL。

如果要使用项目根 `.env.test` 的 JWT 配置现场生成测试 token，并部署这个测试页面，用：

```bash
cd frontend-test
pnpm run deploy:cloudflare -- --api-base-url https://your-pay3-api.example
```

也可以直接跑 shell 脚本：

```bash
bash scripts/deploy-frontend-test-cloudflare.sh --api-base-url https://your-pay3-api.example
```

先 dry-run：

```bash
cd frontend-test
pnpm run deploy:cloudflare -- --api-base-url https://your-pay3-api.example --dry-run
```

这个脚本要求后端 API URL 由命令行或 shell 环境显式传入，不会从 `.env.test` 读取 `PAY3_API_BASE_URL`。脚本读取 `../.env.test` 里的 `JWT_SECRET`、`JWT_ISSUER`、`JWT_AUDIENCE`、`JWT_KEY_ID`/`JWT_KID` 生成只含订单测试 scope 的 JWT；如果这些签名配置不存在，则会使用 `.env.test` 或 shell 里的 `PAY3_API_JWT` / `PAY3_TEST_JWT`。随后把这些值作为 Pages secret 上传，再部署 `public/`：

- `PAY3_TEST_JWT`
- `PAY3_TEST_JWT_EXPIRES_AT`
- `PAY3_API_BASE_URL`
- `PAY3_DEFAULT_MODE`
- `PAY3_PROXY_ENABLED`
- `PAY3_DIRECT_ENABLED`
- `PAY3_EXPOSE_TEST_JWT`

关闭浏览器直连并不暴露 token：

```bash
pnpm run deploy:cloudflare -- --api-base-url https://your-pay3-api.example --no-direct --hide-token
```

## 直连开关

默认调用模式是 Pages Function 代理：

- 页面调用同源 `/api/pay3/*`。
- Pages Function 注入 `PAY3_TEST_JWT`。
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
pnpm run token
```

生产或长期公开环境不要把 `PAY3_TEST_JWT` 放在 `wrangler.toml`；改用 `wrangler secret put PAY3_TEST_JWT`，并关闭 `PAY3_EXPOSE_TEST_JWT`。
