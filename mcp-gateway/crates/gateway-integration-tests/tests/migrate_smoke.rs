use std::fs;

use gateway_core::migrate_v1_to_v2_file;
use tempfile::NamedTempFile;

#[test]
fn migrate_v1_to_v2_smoke() {
    let input = NamedTempFile::new().expect("temp input");
    let output = NamedTempFile::new().expect("temp output");

    fs::write(
        input.path(),
        r#"{
  "version": 1,
  "listen": "127.0.0.1:8765",
  "security": {"mcp": {"enabled": false, "token": ""}, "admin": {"enabled": true, "token": "abc"}},
  "transport": {"streamableHttp": {"basePath": "/mcp"}, "sse": {"basePath": "/sse"}},
  "defaults": {"lifecycle": "pooled", "idleTtlMs": 300000, "requestTimeoutMs": 60000, "maxRetries": 2},
  "servers": [{"name": "fs", "describe": "Filesystem", "command": "npx", "args": []}]
}"#,
    )
    .expect("write input");

    let cfg = migrate_v1_to_v2_file(input.path(), output.path()).expect("migrate");
    assert_eq!(cfg.version, 2);
    assert_eq!(cfg.servers[0].description, "Filesystem");
}
