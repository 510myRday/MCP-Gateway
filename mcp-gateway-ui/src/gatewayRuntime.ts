import { invoke } from "@tauri-apps/api/core";

export interface GatewayProcessStatus {
  running: boolean;
  pid: number | null;
  launchedByUi: boolean;
  executable: string | null;
  configPath: string | null;
  lastError: string | null;
}

const EMPTY_STATUS: GatewayProcessStatus = {
  running: false,
  pid: null,
  launchedByUi: false,
  executable: null,
  configPath: null,
  lastError: null,
};

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function getGatewayStatus(): Promise<GatewayProcessStatus> {
  if (!isTauriRuntime()) {
    return EMPTY_STATUS;
  }
  return invoke<GatewayProcessStatus>("gateway_status");
}

export async function startGateway(): Promise<GatewayProcessStatus> {
  if (!isTauriRuntime()) {
    return EMPTY_STATUS;
  }
  return invoke<GatewayProcessStatus>("start_gateway");
}

export async function stopGateway(): Promise<GatewayProcessStatus> {
  if (!isTauriRuntime()) {
    return EMPTY_STATUS;
  }
  return invoke<GatewayProcessStatus>("stop_gateway");
}

