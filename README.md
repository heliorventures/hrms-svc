# kabipay-svc

Rust workspace for **kabipay-auth** (REST, JWT), **kabipay-ops** (unified ops GraphQL) plus **tenant-plane** async-GraphQL subgraphs (HRMS domains), a shared **outbox** background worker, and library crates **kabipay-common** + **kabipay-db-entities** (SeaORM). Each HTTP service listens on its own port; **kabipay-gateway** stitches the subgraphs into one federated GraphQL endpoint (see `kabipay-gateway`).

## Architecture

| Layer | Contents |
|-------|----------|
| **Control / ops plane** | `kabipay-ops` — single GraphQL service for `kabipay_ops` (tenants, modules, subscriptions, feature flags, operators, billing). |
| **Client / tenant plane** | Employee, leave, attendance, payroll, tax, benefits, expense, recruitment, performance, lms, succession, compensation, assets, grievance, workflow, notification — one **tenant schema** per customer (`tenant_<uuid>`), resolved via `kabipay_ops.tenant_database`. |
| **Auth** | `kabipay-auth` — REST login/refresh, HS256 for client and operator planes (separate secrets). |
| **Async integration** | `kabipay-outbox-worker` — polls `outbox_event` in tenant DBs; **no GraphQL** (CLI process). |
| **Data** | Single PostgreSQL database: Liquibase **ops** changelog once, **tenant** changelog per provisioned tenant (see `kabipay-database`). |

**Postgres access:** prefer **`DATABASE_URL`** (or `POSTGRES_*` building a DSN). For **Neon** and other serverless PG, use the **`*-pooler`** host in `DATABASE_URL` to keep connection count and active compute down; set `POSTGRES_SSLMODE=require` when the provider needs TLS. Per-process pool caps (`KABIPAY_DB_POOL_MAX`, `KABIPAY_TENANT_DB_POOL_MAX`, optional `KABIPAY_DB_IDLE_TIMEOUT_SECS`) are documented in `.env.example`.

## Dependencies

| Requirement | Notes |
|-------------|--------|
| **Rust** | Stable toolchain (`rustup`, `cargo`). |
| **Windows MSVC linker** | Default **`windows-msvc`** needs **`link.exe`** — install **[Visual Studio Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)** workload **Desktop development with C++**. **`link.exe` not found** → add this before `cargo build`. |
| **PostgreSQL 16** | **Ops** schema `kabipay_ops` + per-**tenant** schemas; apply migrations with **kabipay-database** (Liquibase). |
| **Environment** | Copy `.env.example` → `.env`. Managed DBs: TLS + pooler URL as above. |

Optional:

- **PowerShell** (Windows) for `scripts/*.ps1` helpers.

## Configure

1. Ensure Postgres is running and migrations are applied (see **kabipay-database** README: ops changelog first, then tenant schema(s)).

2. In this directory:

   ```powershell
   copy .env.example .env
   ```

   Edit `.env`:

   - Set **`DATABASE_URL`** (recommended) *or* **`POSTGRES_HOST` / `POSTGRES_PORT` / `POSTGRES_DB` / `POSTGRES_USER` / `POSTGRES_PASSWORD`** (and **`POSTGRES_SSLMODE=require`** for Neon, Aiven, etc.). Use the **pooler** hostname for Neon when you want pooled server-side sessions.
   - Set **`KABIPAY_CLIENT_JWT_SECRET`** and **`KABIPAY_OPERATOR_JWT_SECRET`** to long random strings (32+ characters).
   - For many processes against a small DB, set **`KABIPAY_DB_POOL_MAX=1`** and **`KABIPAY_TENANT_DB_POOL_MAX=1`** (see `.env.example`).

## Build

From this directory:

```powershell
cargo build --workspace
```

On memory-constrained Windows machines, build crates one at a time if needed:

```powershell
cargo build -j 1 -p kabipay-auth
```

## Run services

### Auth (REST)

```powershell
cargo run -p kabipay-auth
```

Default port: **`4001`** (`KABIPAY_AUTH_PORT`).

### One subgraph (example)

```powershell
cargo run -p kabipay-employee
```

GraphQL: **`http://127.0.0.1:4013/graphql`** (port from `KABIPAY_EMPLOYEE_PORT` in `.env`).

### All subgraphs (Windows helper)

After a **release** or **debug** build, from this directory:

```powershell
.\scripts\start-subgraphs.ps1
```

The script starts **debug** binaries under `target\debug\kabipay-*.exe`: **ops** on **4010**, tenant modules **4013–4029**. Unless you set `KABIPAY_DB_POOL_MAX` / `KABIPAY_TENANT_DB_POOL_MAX` in your shell, it defaults both to **1** so many processes can share a small managed Postgres `max_connections` limit (otherwise startup hits “pool timed out” when the DB refuses new connections). Ensure **`kabipay-auth`** is started separately if the UI or gateway needs login.

## Scripts (optional)

| Script | Purpose |
|--------|---------|
| `scripts\start-subgraphs.ps1` | Starts the Rust subgraph binaries for local service runtime. |
| `cargo run -p kabipay-outbox-worker` | Outbox poller (same DB as subgraphs; configure `OUTBOX_*` in `.env.example`). |

Database setup, tenant provisioning, tenant Liquibase updates, and demo seed data are owned by **kabipay-database**.

## Services and ports (defaults)

| Crate / binary | Role | Env var | Default port |
|----------------|------|---------|--------------|
| `kabipay-auth` | REST (login, tokens) | `KABIPAY_AUTH_PORT` | 4001 |
| `kabipay-ops` | GraphQL (ops — tenants, operators, billing) | `KABIPAY_OPS_PORT` | 4010 |
| `kabipay-employee` | GraphQL | `KABIPAY_EMPLOYEE_PORT` | 4013 |
| `kabipay-leave` | GraphQL | `KABIPAY_LEAVE_PORT` | 4014 |
| `kabipay-attendance` | GraphQL | `KABIPAY_ATTENDANCE_PORT` | 4015 |
| `kabipay-payroll` | GraphQL | `KABIPAY_PAYROLL_PORT` | 4016 |
| `kabipay-tax` | GraphQL | `KABIPAY_TAX_PORT` | 4017 |
| `kabipay-benefits` | GraphQL | `KABIPAY_BENEFITS_PORT` | 4018 |
| `kabipay-expense` | GraphQL | `KABIPAY_EXPENSE_PORT` | 4019 |
| `kabipay-recruitment` | GraphQL | `KABIPAY_RECRUITMENT_PORT` | 4020 |
| `kabipay-performance` | GraphQL | `KABIPAY_PERFORMANCE_PORT` | 4021 |
| `kabipay-lms` | GraphQL | `KABIPAY_LMS_PORT` | 4022 |
| `kabipay-succession` | GraphQL | `KABIPAY_SUCCESSION_PORT` | 4023 |
| `kabipay-compensation` | GraphQL | `KABIPAY_COMPENSATION_PORT` | 4024 |
| `kabipay-assets` | GraphQL | `KABIPAY_ASSETS_PORT` | 4025 |
| `kabipay-grievance` | GraphQL | `KABIPAY_GRIEVANCE_PORT` | 4026 |
| `kabipay-workflow` | GraphQL | `KABIPAY_WORKFLOW_PORT` | 4027 |
| `kabipay-notification` | GraphQL | `KABIPAY_NOTIFICATION_PORT` | 4028 |
| `kabipay-analytics` | GraphQL | `KABIPAY_ANALYTICS_PORT` | 4029 |
| `kabipay-outbox-worker` | Background worker (no HTTP) | — | — |

Stitched URLs: `http://127.0.0.1:<port>/graphql`. Canonical list for the gateway: `kabipay-gateway/src/subgraphs.ts`. `start-subgraphs.ps1` starts **ops** plus tenant-module GraphQL executables (4010, 4013–4029); run **auth** and **outbox** separately.

## Related repositories

- **kabipay-database** — Liquibase schema migrations.
- **kabipay-gateway** — federated GraphQL gateway; point it at the same subgraph base URL and ports.
- **kabipay-ui** — browser client; `public/config.json` must match auth URL and gateway URL.
