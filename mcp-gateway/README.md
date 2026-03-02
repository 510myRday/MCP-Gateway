# MCP Gateway (V2)

This repository is the backend split-out for MCP Gateway.

## Workspace Layout

- `crates/gateway-core`: config model, config service, protocol/runtime process manager
- `crates/gateway-http`: Axum HTTP API, auth middleware, SSE bridge, OpenAPI generation
- `crates/gateway-cli`: CLI entry point (`gateway` binary)
- `crates/gateway-integration-tests`: black-box integration tests

## API Contract

- Base prefix: `/api/v2`
- Streamable HTTP: `POST /api/v2/mcp/{server_name}`
- SSE subscribe/request: `GET|POST /api/v2/sse/{server_name}`
- Admin API: `/api/v2/admin/*`
- OpenAPI: `/api/v2/openapi.json`
- Swagger UI: `/api/v2/docs`

All responses use envelope:

```json
{
  "ok": true,
  "data": {},
  "requestId": "uuid"
}
```

Error responses:

```json
{
  "ok": false,
  "error": {
    "code": "VALIDATION_FAILED",
    "message": "..."
  },
  "requestId": "uuid"
}
```

## Quick Start

```bash
cp config.example.json ./config.v2.json
cargo run -p gateway-cli -- run --config ./config.v2.json
```

## CLI

```bash
gateway run --config <path> --mode <extension|general|both> --listen <addr>
gateway init --config <path> --mode <extension|general|both>
gateway validate --config <path>
gateway token rotate --scope <admin|mcp> --config <path>
gateway migrate-config --from v1 --to v2 --input <old> --output <new>
```

## Quality Gates

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```