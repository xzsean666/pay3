# TASK-034: 统一本地与远程 SSH 自动化部署与无损升级脚本

状态: `DONE`
依赖: [TASK-032](file:///ssd0/git/pay3/docs/AI/tasks/TASK-032.md), [TASK-033](file:///ssd0/git/pay3/docs/AI/tasks/TASK-033.md)
完成时间: 2026-10-04
完成情况:
- [x] 编写通用部署脚本 `scripts/deploy.sh`，支持本地与远程 SSH 部署；
- [x] 支持传入目标上级部署目录并在其下建立 `pay3` 子目录部署，本地部署模式默认免输入文件夹；
- [x] 支持传入必要的 `.env` 文件与 `pay3.yaml` 配置文件，并安全挂载与权限保护；
- [x] 基于 `rsync` 增量同步代码，明确排除 `data/` 持久化目录，确保重复部署为无损平滑升级；
- [x] 集成容器原地重构升级（`docker compose up -d --build`）、数据库自动迁移与 `/readyz` 健康检查。

---

## 1. 任务目标

为 Pay3 提供生产级、开箱即用的自动化部署与无损升级脚本 `scripts/deploy.sh`：
1. **灵活运行模式**: 支持本地直接部署（默认使用当前仓库，不强制要求传入目录参数）与通过 SSH + rsync 部署到远程服务器。
2. **规范部署目录**: 当提供部署目录参数时，自动在目标目录下创建 `pay3` 子目录作为部署根目录（例如传入 `/opt` 则自动部署到 `/opt/pay3`）。
3. **配置与机密管理**: 允许用户传入指定的 `.env` 环境变量文件及 `pay3.yaml` 配置文件，并将其安全部署到目标目录，自动处理 Docker Compose 挂载（通过 `docker-compose.override.yml`）与敏感文件权限（`chmod 600 .env`）。
4. **无损平滑升级**: 重新运行部署脚本即为版本升级；通过 `rsync` 安全排除 `data/` 目录，禁止 `docker compose down -v` 或删除数据卷，保留 PostgreSQL 持久化数据；容器拉起时自动触发 SQLx schema migrations。
5. **部署后自检**: 部署后自动轮询容器状态与 `/readyz` 深度就绪检查，确保服务健康。

---

## 2. 验收标准

1. `scripts/deploy.sh` 具备完整命令行参数解析（`--mode`, `--remote`, `-p`, `-i`, `-d`, `-e`, `-c`, `--prebuilt`, `--cn`, `--local-db`, `--skip-healthcheck`, `--dry-run` 等）并提供友好 `--help` 提示。
2. 语法静态检查 `bash -n scripts/deploy.sh` 无错误。
3. 本地模式执行时不强制要求输入目录参数，默认直接定位当前项目。
4. 远程 SSH 模式下能基于 `rsync` 排除构建缓存与 `data/`，绝不清空既有数据。
5. 更新 `docs/DEPLOYMENT.md` 记录详细使用说明与实操样例。
