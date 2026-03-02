import { invoke } from "@tauri-apps/api/core";
import type { GatewayConfig } from "./types";

export async function loadLocalConfig(): Promise<GatewayConfig> {
  return invoke<GatewayConfig>("load_local_config");
}

export async function saveLocalConfig(config: GatewayConfig): Promise<void> {
  await invoke("save_local_config", { config });
}

export async function getConfigPath(): Promise<string> {
  return invoke<string>("get_config_path");
}

