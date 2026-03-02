export type JsonPrimitive = string | number | boolean | null;
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue };

export interface TokenConfig {
  enabled: boolean;
  token: string;
}

export interface SecurityConfig {
  mcp: TokenConfig;
  admin: TokenConfig;
}

export interface TransportPath {
  basePath: string;
}

export interface TransportConfig {
  streamableHttp: TransportPath;
  sse: TransportPath;
}

export interface DefaultsConfig {
  lifecycle: "pooled" | "per_request";
  idleTtlMs: number;
  requestTimeoutMs: number;
  maxRetries: number;
  maxResponseWaitIterations: number;
}

export interface ServerConfig {
  name: string;
  description: string;
  command: string;
  args: string[];
  cwd: string;
  env: Record<string, string>;
  lifecycle: "pooled" | "per_request" | null;
  stdioProtocol: "auto" | "content_length" | "json_lines";
  enabled: boolean;
}

export interface GatewayConfig {
  version: number;
  listen: string;
  allowNonLoopback: boolean;
  mode: "extension" | "general" | "both";
  security: SecurityConfig;
  transport: TransportConfig;
  defaults: DefaultsConfig;
  servers: ServerConfig[];
}

export interface HealthData {
  startedAt: string;
  uptimeSeconds: number;
  mode: string;
  listen: string;
  serverCount: number;
  version: string;
}

export interface ApiErrorBody {
  code: string;
  message: string;
  details?: JsonValue;
}

export interface ApiEnvelope<T> {
  ok: boolean;
  data?: T;
  error?: ApiErrorBody;
  requestId: string;
}

export interface ToolListResult {
  refresh: boolean;
  result: JsonValue;
}

export interface ExportPayload {
  mcpServers: Record<string, JsonValue>;
}