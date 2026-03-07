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

export interface SkillsPolicyConfig {
  defaultAction: SkillPolicyAction;
  rules: SkillCommandRule[];
  pathGuard: SkillsPathGuardConfig;
}

export interface SkillsExecutionConfig {
  timeoutMs: number;
  maxOutputBytes: number;
}

export type SkillPolicyAction = "allow" | "confirm" | "deny";

export interface SkillsPathGuardConfig {
  enabled: boolean;
  whitelistDirs: string[];
  onViolation: SkillPolicyAction;
}

export interface SkillCommandRule {
  id: string;
  action: SkillPolicyAction;
  commandTree: string[];
  contains: string[];
  reason: string;
}

export interface SkillRootEntry {
  path: string;
  enabled: boolean;
}

export interface SkillsConfig {
  enabled: boolean;
  serverName: string;
  roots: string[];
  rootEntries?: SkillRootEntry[];
  policy: SkillsPolicyConfig;
  execution: SkillsExecutionConfig;
}

export interface ServerConfig {
  name: string;
  description: string;
  command: string;
  args: string[];
  cwd: string;
  env: Record<string, string>;
  lifecycle: "pooled" | "per_request" | null;
  stdioProtocol: "auto";
  enabled: boolean;
}

export interface GatewayConfig {
  version: number;
  listen: string;
  allowNonLoopback: boolean;
  mode: "extension" | "general" | "both";
  apiPrefix: string;
  security: SecurityConfig;
  transport: TransportConfig;
  defaults: DefaultsConfig;
  servers: ServerConfig[];
  skills: SkillsConfig;
}

export interface HealthData {
  startedAt: string;
  uptimeSeconds: number;
  mode: string;
  listen: string;
  serverCount: number;
  version: string;
}

export interface ServerConnectivityTestResult {
  ok: boolean;
  status?: "ok" | "auth_required";
  message?: string;
  authUrl?: string;
  browserOpened?: boolean;
  waitingForAuthorization?: boolean;
  initialize?: JsonValue;
  testedAt: string;
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

export type ConfirmationStatus = "pending" | "approved" | "rejected";

export interface SkillConfirmation {
  id: string;
  status: ConfirmationStatus;
  createdAt: string;
  updatedAt: string;
  skill: string;
  script: string;
  args: string[];
  commandPreview: string;
  reason: string;
}

export interface SkillSummary {
  skill: string;
  description: string;
  root: string;
  path: string;
  hasScripts: boolean;
}

export interface SkillDirectoryValidation {
  exists: boolean;
  isDir: boolean;
  hasSkillMd: boolean;
}
