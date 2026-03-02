# MCP Gateway UI

Frontend split-out repository for MCP Gateway.

## Scripts

```bash
npm ci
npm run typecheck
npm run test
npm run build
npm run tauri dev
```

## Runtime Contract

The UI consumes backend API v2 endpoints under `/api/v2/*` and expects envelope responses:

```json
{
  "ok": true,
  "data": {},
  "requestId": "uuid"
}
```