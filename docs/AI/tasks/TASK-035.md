# TASK-035: 支持 Signer 独立部署与 Pay3 + Signer 一体化极简部署

状态: `DONE`
依赖: [TASK-034](file:///ssd0/git/pay3/docs/AI/tasks/TASK-034.md)
完成时间: 2026-10-04
完成情况:
- [x] 在 `docker-compose.yml`、`docker-compose.prebuilt.yml`、`docker-compose.prebuilt.cn.yml` 中新增 `signer` 服务并分配 `signer` profile；
- [x] 升级 `scripts/deploy.sh` 支持 `--with-signer`（一体化全量部署）与 `--signer-only`（独立签名机部署）模式；
- [x] 实现针对 `signer` 服务的专用健康自检（`/healthz`）与动态 Compose Profile 注入；
- [x] 更新 `docs/DEPLOYMENT.md` 生产一键一体化部署与 Signer 独立部署使用示例。

---

## 1. 任务目标

满足用户在生产或高安全隔离场景下的多样化部署诉求：
1. **一体化全量部署 (`--with-signer` / `--service all`)**：
   在单台服务器上一键部署 `pay3` + `signer` (+ 可选 `postgres`) 完整环境。`signer` 仅在 Docker 内部局域网运行，与 `pay3` 形成安全网络隔离，同时满足用户“一条命令一次性全部部署完”的核心诉求。
2. **Signer 独立部署 (`--signer-only` / `--service signer`)**：
   支持将 Signer 单独部署到高安全级别的专用签名机服务器上。
3. **独立 Pay3 网关部署 (`--service pay3`，默认)**：
   保持现有默认行为不变，仅部署 Pay3 网关。

---

## 2. 验收标准

1. `docker compose config`（包含 `--profile signer`）语法正确无报错；
2. `scripts/deploy.sh` 具备 `--with-signer` 与 `--signer-only` 选项，且 `--help` 提示完整；
3. 本地 dry-run 模拟三种部署模式，均能输出正确的 Docker Compose 服务启动列表与健康检查脚本；
4. 更新 `docs/DEPLOYMENT.md` 文档。
