PAY3_RUN_REAL_CHAIN_E2E=1 cargo test --test real_chain_e2e -- --ignored --nocapture
PAY3_RUN_REAL_CHAIN_E2E=1 PAY3_E2E_CONCURRENCY=10 cargo test --test real_chain_e2e -- --ignored --nocapture

PAY3_ENV_FILE=.env.test docker compose up -d
PAY3_ENV_FILE=.env.test docker compose --env-file .env.test up -d --build pay3

PAY3_ENV_FILE=.env.test docker compose \
    --env-file .env.test \
    -f docker-compose.prebuilt.yml \
    up -d --build pay3

docker compose \
    --env-file .env.test \
    -f docker-compose.prebuilt.yml \
    down


PAY3_ENV_FILE=.env.test docker compose \
    --env-file .env.test \
    -f docker-compose.prebuilt.cn.yml \
    up -d --build pay3


PAY3_RUN_DEPLOYED_API_ACCEPTANCE=1 \
PAY3_DEPLOYED_API_ENV_FILE=.env.test \
PAY3_API_BASE_URL=https://sean-10030.002788.xyz/ \
cargo test --test deployed_api_acceptance -- --ignored --nocapture