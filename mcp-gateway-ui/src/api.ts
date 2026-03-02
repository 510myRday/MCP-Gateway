import type {
  ApiEnvelope,
  ExportPayload,
  GatewayConfig,
  HealthData,
  JsonValue,
  ServerConfig,
  ToolListResult,
} from "./types";

export function normalizeBaseUrl(baseUrl: string): string {
  const normalized = baseUrl.trim();
  const withScheme = /^[a-zA-Z][a-zA-Z\d+\-.]*:\/\//.test(normalized)
    ? normalized
    : `http://${normalized}`;
  return withScheme.replace(/\/+$/, "");
}

const REQUEST_TIMEOUT_MS = 6000;

function toJsonValue<T>(value: T): JsonValue {
  return JSON.parse(JSON.stringify(value)) as JsonValue;
}

export class ApiClient {
  private baseUrl: string;
  private adminToken: string;
  private apiPrefix: string;

  constructor(baseUrl: string, adminToken: string, apiPrefix: string = "/api/v2") {
    this.baseUrl = normalizeBaseUrl(baseUrl);
    this.adminToken = adminToken.trim();
    this.apiPrefix = apiPrefix.startsWith('/') ? apiPrefix : `/${apiPrefix}`;
  }

  setAdminToken(token: string) {
    this.adminToken = token.trim();
  }

  getAdminToken(): string {
    return this.adminToken;
  }

  private async request<T>(method: string, path: string, body?: JsonValue): Promise<T> {
    const headers: Record<string, string> = {
      Accept: "application/json",
    };

    if (body !== undefined) {
      headers["Content-Type"] = "application/json";
    }

    if (this.adminToken) {
      headers.Authorization = `Bearer ${this.adminToken}`;
    }

    // 构建完整路径：baseUrl + apiPrefix + path
    const fullPath = `${this.baseUrl}${this.apiPrefix}${path}`;

    let response: Response;
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);
    try {
      response = await fetch(fullPath, {
        method,
        headers,
        body: body !== undefined ? JSON.stringify(body) : undefined,
        signal: controller.signal,
      });
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") {
        throw new Error("Gateway request timed out");
      }
      throw new Error("Cannot connect to gateway server");
    } finally {
      clearTimeout(timeout);
    }

    const text = await response.text();
    let payload: ApiEnvelope<T> | null = null;

    if (text.trim().length > 0) {
      try {
        payload = JSON.parse(text) as ApiEnvelope<T>;
      } catch {
        throw new Error(`Gateway returned non-JSON response (HTTP ${response.status})`);
      }
    }

    if (!payload) {
      throw new Error("Gateway returned empty response");
    }

    if (!response.ok || payload.ok === false) {
      throw new Error(payload.error?.message || `HTTP ${response.status} ${response.statusText}`);
    }

    if (payload.data === undefined) {
      throw new Error("Gateway response does not contain data");
    }

    return payload.data;
  }

  async getHealth(): Promise<HealthData> {
    return this.request<HealthData>("GET", "/api/v2/admin/health");
  }

  async getConfig(): Promise<GatewayConfig> {
    return this.request<GatewayConfig>("GET", "/api/v2/admin/config");
  }

  async updateConfig(config: GatewayConfig): Promise<GatewayConfig> {
    return this.request<GatewayConfig>("PUT", "/api/v2/admin/config", toJsonValue(config));
  }

  async getServers(): Promise<ServerConfig[]> {
    return this.request<ServerConfig[]>("GET", "/api/v2/admin/servers");
  }

  async addServer(server: ServerConfig): Promise<ServerConfig> {
    return this.request<ServerConfig>("POST", "/api/v2/admin/servers", toJsonValue(server));
  }

  async updateServer(name: string, server: ServerConfig): Promise<ServerConfig> {
    return this.request<ServerConfig>(
      "PUT",
      `/api/v2/admin/servers/${encodeURIComponent(name)}`,
      toJsonValue(server),
    );
  }

  async deleteServer(name: string): Promise<{ deleted: string }> {
    return this.request<{ deleted: string }>("DELETE", `/api/v2/admin/servers/${encodeURIComponent(name)}`);
  }

  async testServer(name: string): Promise<JsonValue> {
    return this.request<JsonValue>("POST", `/api/v2/admin/servers/${encodeURIComponent(name)}/test`);
  }

  async getServerTools(name: string, refresh = true): Promise<ToolListResult> {
    return this.request<ToolListResult>(
      "GET",
      `/api/v2/admin/servers/${encodeURIComponent(name)}/tools?refresh=${refresh}`,
    );
  }

  async exportConfig(): Promise<ExportPayload> {
    return this.request<ExportPayload>("GET", "/api/v2/admin/export/mcp-servers");
  }
}
