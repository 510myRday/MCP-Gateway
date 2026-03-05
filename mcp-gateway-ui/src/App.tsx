import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import {
  Play,
  Square,
  Copy,
  Check,
  Code2,
  List,
  Languages,
  Save,
  FolderOpen,
  Eye,
  EyeOff,
  Globe,
  Github,
  Send,
} from "lucide-react";
import { open } from "@tauri-apps/plugin-shell";
import { getGatewayStatus, startGateway, stopGateway, type GatewayProcessStatus } from "./gatewayRuntime";
import {
  loadLocalConfig,
  saveLocalConfig,
  getConfigPath,
  getDefaultSkillRules,
  testMcpServerLocal,
  pickFolderDialog,
  validateSkillDirectory,
  focusMainWindowForSkillConfirmation,
} from "./localConfig";
import { ApiClient } from "./api";
import { usePolling, type PollOutcome } from "./hooks/usePolling";
import type {
  GatewayConfig,
  ServerConfig,
  SkillCommandRule,
  SkillConfirmation,
  ServerConnectivityTestResult,
  SkillDirectoryValidation,
  SkillRootEntry,
  SkillPolicyAction,
  SkillsConfig,
} from "./types";
import { useT, type Lang } from "./i18n";
import JsonEditor from "./components/JsonEditor";

// ── 工具：args 字符串 ↔ 数组 ──────────────────────────────────────
function argsToStr(args: string[]): string {
  return args.map((a) => (a.includes(" ") ? `"${a}"` : a)).join(" ");
}
function strToArgs(raw: string): string[] {
  return raw.match(/(?:[^\s"]+|"[^"]*")+/g)?.map((a) => a.replace(/^"|"$/g, "")) ?? [];
}

// ── servers → claude_desktop_config 格式的 JSON 对象 ─────────────
function serversToJson(servers: ServerConfig[]): Record<string, unknown> {
  const obj: Record<string, unknown> = {};
  for (const s of servers) {
    obj[s.name || `server_${Math.random().toString(36).slice(2, 6)}`] = {
      command: s.command,
      args: s.args,
      ...(s.env && Object.keys(s.env).length > 0 ? { env: s.env } : {}),
    };
  }
  return obj;
}

// ── claude_desktop_config 格式 → servers ─────────────────────────
function jsonToServers(obj: Record<string, unknown>): ServerConfig[] {
  return Object.entries(obj).map(([name, val]) => {
    const v = val as { command?: string; args?: string[]; env?: Record<string, string> };
    return {
      name,
      command: v.command ?? "",
      args: v.args ?? [],
      env: v.env ?? {},
      description: "",
      cwd: "",
      lifecycle: null,
      stdioProtocol: "auto" as const,
      enabled: true,
    };
  });
}

function parseRulesJson(input: string): SkillCommandRule[] {
  const parsed = JSON.parse(input) as unknown;
  if (!Array.isArray(parsed)) {
    throw new Error("rules must be an array");
  }
  return parsed.map((entry, index) => {
    if (!entry || typeof entry !== "object") {
      throw new Error(`rule ${index + 1} must be object`);
    }
    const row = entry as Record<string, unknown>;
    const action = typeof row.action === "string" ? row.action : "allow";
    if (action !== "allow" && action !== "confirm" && action !== "deny") {
      throw new Error(`rule ${index + 1} action must be allow/confirm/deny`);
    }

    return {
      id: typeof row.id === "string" ? row.id : `rule-${index + 1}`,
      action: action as SkillPolicyAction,
      commandTree: Array.isArray(row.commandTree) ? row.commandTree.map((item) => String(item)) : [],
      contains: Array.isArray(row.contains) ? row.contains.map((item) => String(item)) : [],
      reason: typeof row.reason === "string" ? row.reason : "",
    };
  });
}

function isConfirmationAlreadyResolvedError(error: unknown): boolean {
  const message = String(error ?? "").toLowerCase();
  return message.includes("confirmation not found")
    || message.includes("already rejected")
    || message.includes("already approved");
}

function ensureSkillsConfig(
  raw: Partial<SkillsConfig> | undefined,
  fallbackRules: SkillCommandRule[] = [],
): SkillsConfig {
  const legacyPolicy = (raw?.policy as unknown as {
    confirmKeywords?: string[];
    denyKeywords?: string[];
  } | undefined);
  const normalizedRoots = Array.isArray(raw?.roots)
    ? raw.roots.map((item) => item.trim()).filter((item) => item.length > 0)
    : [];
  const parsedRootEntries = Array.isArray(raw?.rootEntries)
    ? raw.rootEntries
        .map((entry) => ({
          path: (entry?.path ?? "").trim(),
          enabled: entry?.enabled === true,
        }))
        .filter((entry) => entry.path.length > 0)
    : [];
  const rootEntries: SkillRootEntry[] = parsedRootEntries.length > 0
    ? [...parsedRootEntries]
    : normalizedRoots.map((path) => ({ path, enabled: true }));

  const knownPaths = new Set(rootEntries.map((entry) => entry.path.toLowerCase()));
  normalizedRoots.forEach((path) => {
    if (!knownPaths.has(path.toLowerCase())) {
      rootEntries.push({ path, enabled: true });
      knownPaths.add(path.toLowerCase());
    }
  });

  const hasLegacy = Array.isArray(legacyPolicy?.confirmKeywords) || Array.isArray(legacyPolicy?.denyKeywords);
  let rules = Array.isArray(raw?.policy?.rules) && raw.policy.rules.length > 0
    ? raw.policy.rules
    : fallbackRules;

  if ((!raw?.policy?.rules || raw.policy.rules.length === 0) && hasLegacy) {
    const legacyConfirm = legacyPolicy?.confirmKeywords ?? [];
    const legacyDeny = legacyPolicy?.denyKeywords ?? [];
    const legacyRules: SkillCommandRule[] = [];
    legacyDeny.forEach((item, index) => {
      if (item.trim().length > 0) {
        legacyRules.push({
          id: `legacy-deny-${index + 1}`,
          action: "deny",
          commandTree: [],
          contains: [item],
          reason: `Legacy deny keyword: ${item}`,
        });
      }
    });
    legacyConfirm.forEach((item, index) => {
      if (item.trim().length > 0) {
        legacyRules.push({
          id: `legacy-confirm-${index + 1}`,
          action: "confirm",
          commandTree: [],
          contains: [item],
          reason: `Legacy confirm keyword: ${item}`,
        });
      }
    });
    if (legacyRules.length > 0) {
      rules = legacyRules;
    }
  }

  return {
    enabled: raw?.enabled ?? false,
    serverName: raw?.serverName?.trim() || "__skills__",
    roots: rootEntries.filter((entry) => entry.enabled).map((entry) => entry.path),
    rootEntries,
    policy: {
      defaultAction: raw?.policy?.defaultAction ?? "allow",
      rules: rules.map((rule) => ({
        id: (rule.id ?? "").trim(),
        action: rule.action ?? "allow",
        commandTree: Array.isArray(rule.commandTree)
          ? rule.commandTree.map((item) => item.trim()).filter((item) => item.length > 0)
          : [],
        contains: Array.isArray(rule.contains)
          ? rule.contains.map((item) => item.trim()).filter((item) => item.length > 0)
          : [],
        reason: (rule.reason ?? "").trim(),
      })),
      pathGuard: {
        enabled: raw?.policy?.pathGuard?.enabled ?? false,
        whitelistDirs: Array.isArray(raw?.policy?.pathGuard?.whitelistDirs)
          ? raw.policy.pathGuard.whitelistDirs.map((item) => item.trim()).filter((item) => item.length > 0)
          : [],
        onViolation: raw?.policy?.pathGuard?.onViolation ?? "confirm",
      },
    },
    execution: {
      timeoutMs: raw?.execution?.timeoutMs ?? 30000,
      maxOutputBytes: raw?.execution?.maxOutputBytes ?? 131072,
    },
  };
}

function formatTime(value: string): string {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) {
    return value;
  }
  return parsed.toLocaleString();
}

interface EditableConfigSnapshot {
  servers: ServerConfig[];
  listen: string;
  apiPrefix: string;
  transport: GatewayConfig["transport"];
  security: GatewayConfig["security"];
  skills: SkillsConfig;
}

type EndpointTransportType = "sse" | "streamable-http";

const BLOG_URL = "https://blog.aiguicai.com";
const GITHUB_URL = "https://github.com/510myRday/MCP-Gateway";
const TG_GROUP_URL = "https://t.me/+vq8WByYtPoQ1MjA1";
const QQ_GROUP_NUMBER = "1090461840";
// 最稳方式：填入群分享得到的官方邀请链接（含 k / idkey 等参数）
// 例如：https://qm.qq.com/cgi-bin/qm/qr?k=xxxx 或 https://shang.qq.com/wpa/qunwpa?idkey=xxxx
const QQ_GROUP_INVITE_URL = "https://qm.qq.com/q/rsa3XRgFe8";
const QQ_GROUP_PROTOCOL_LINKS = [
  `tencent://GroupProfile?groupid=${QQ_GROUP_NUMBER}`,
  `mqqapi://card/show_pslcard?src_type=internal&version=1&uin=${QQ_GROUP_NUMBER}&card_type=group&source=qrcode`,
] as const;

function QqLogoIcon({ size = 14 }: { size?: number }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 448 512"
      fill="currentColor"
      aria-hidden="true"
    >
      <path d="M433.754 420.445c-11.526 1.393-44.86-52.741-44.86-52.741 0 31.345-16.136 72.247-51.051 101.786 16.842 5.192 54.843 19.167 45.803 34.421-7.316 12.343-125.51 7.881-159.632 4.037-34.122 3.844-152.316 8.306-159.632-4.037-9.045-15.25 28.918-29.214 45.783-34.415-34.92-29.539-51.059-70.445-51.059-101.792 0 0-33.334 54.134-44.859 52.741-5.37-.65-12.424-29.644 9.347-99.704 10.261-33.024 21.995-60.478 40.144-105.779C60.683 98.063 108.982.006 224 0c113.737.006 163.156 96.133 160.264 214.963 18.118 45.223 29.912 72.85 40.144 105.778 21.768 70.06 14.716 99.053 9.346 99.704z" />
    </svg>
  );
}

function stableSortValue(value: unknown): unknown {
  if (Array.isArray(value)) {
    return value.map((item) => stableSortValue(item));
  }
  if (value && typeof value === "object") {
    const sorted: Record<string, unknown> = {};
    Object.entries(value as Record<string, unknown>)
      .sort(([a], [b]) => a.localeCompare(b))
      .forEach(([key, nested]) => {
        sorted[key] = stableSortValue(nested);
      });
    return sorted;
  }
  return value;
}

function createEditableConfigSnapshot(input: {
  servers: ServerConfig[];
  listen: string;
  apiPrefix: string;
  ssePath: string;
  httpPath: string;
  adminToken: string;
  mcpToken: string;
  skills: SkillsConfig;
}): EditableConfigSnapshot {
  return {
    servers: input.servers,
    listen: input.listen,
    apiPrefix: input.apiPrefix,
    transport: {
      sse: { basePath: input.ssePath },
      streamableHttp: { basePath: input.httpPath },
    },
    security: {
      admin: { enabled: input.adminToken !== "", token: input.adminToken },
      mcp: { enabled: input.mcpToken !== "", token: input.mcpToken },
    },
    skills: input.skills,
  };
}

function createEditableConfigFingerprint(snapshot: EditableConfigSnapshot): string {
  return JSON.stringify(stableSortValue(snapshot));
}

function createMcpClientEntryJson(name: string, type: EndpointTransportType, url: string): string {
  return JSON.stringify({
    [name]: {
      type,
      url,
    },
  }, null, 2)
    .split("\n")
    .slice(1, -1)
    .join("\n");
}

type SkillDirStatus = "idle" | "checking" | "valid" | "invalid" | "error";
type SkillDirKind = "roots" | "whitelist";
type ServerTestStatus = "idle" | "testing" | "success" | "failed";

interface ServerTestState {
  status: ServerTestStatus;
  message: string;
  testedAt?: string;
}

function serverTestKey(index: number, server?: Pick<ServerConfig, "name">): string {
  const normalizedName = server?.name?.trim().toLowerCase();
  if (normalizedName) {
    return `name:${normalizedName}`;
  }
  return `idx:${index}`;
}

function asErrorMessage(error: unknown): string {
  return String(error ?? "").replace(/^Error:\s*/, "").trim() || "Unknown error";
}

interface SkillDirectoryItem {
  id: string;
  path: string;
  status: SkillDirStatus;
  enabled: boolean;
}

function createSkillDirectoryItem(path = "", enabled = false): SkillDirectoryItem {
  return {
    id: `dir-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    path,
    status: "idle",
    enabled,
  };
}

function skillDirectoryStatusFromResult(result: SkillDirectoryValidation): SkillDirStatus {
  if (result.hasSkillMd) {
    return "valid";
  }
  return "invalid";
}

// ── 删除确认弹窗组件 ──────────────────────────────────────────────
function ConfirmDialog({ open, title, message, onCancel, onConfirm, t }: {
  open: boolean;
  title: string;
  message: string;
  onCancel: () => void;
  onConfirm: () => void;
  t: ReturnType<typeof useT>;
}) {
  if (!open) return null;
  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div className="modal-content" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">{title}</div>
        <div className="modal-body">
          {message}
        </div>
        <div className="modal-footer">
          <button className="btn btn-secondary" onClick={onCancel}>{t("cancel")}</button>
          <button className="btn btn-danger" onClick={onConfirm}>{t("confirmDelete")}</button>
        </div>
      </div>
    </div>
  );
}

function SkillConfirmations({ pending, busyIds, onApprove, onReject, t }: {
  pending: SkillConfirmation[];
  busyIds: Set<string>;
  onApprove: (id: string) => void;
  onReject: (id: string) => void;
  t: ReturnType<typeof useT>;
}) {
  if (pending.length === 0) {
    return <div className="empty-hint">{t("noSkillPending")}</div>;
  }

  return (
    <div className="skill-confirm-list">
      {pending.map((item) => {
        const busy = busyIds.has(item.id);
        return (
          <div className="skill-confirm-item" key={item.id}>
            <div className="skill-confirm-head">
              <div className="skill-confirm-meta">
                <span className="skill-chip">{item.skill}</span>
                <span className="skill-script">{item.script}</span>
              </div>
              <div className="skill-confirm-actions">
                <button className="btn btn-secondary btn-sm" disabled={busy} onClick={() => onReject(item.id)}>
                  {t("reject")}
                </button>
                <button className="btn btn-start btn-sm" disabled={busy} onClick={() => onApprove(item.id)}>
                  {t("approve")}
                </button>
              </div>
            </div>
            <div className="skill-confirm-row">
              <span className="field-label">{t("commandPreview")}</span>
              <code className="skill-command">{item.commandPreview}</code>
            </div>
            <div className="skill-confirm-row">
              <span className="field-label">{t("confirmReason")}</span>
              <span>{item.reason}</span>
            </div>
            <div className="skill-confirm-row">
              <span className="field-label">{t("createdAt")}</span>
              <span>{formatTime(item.createdAt)}</span>
            </div>
          </div>
        );
      })}
    </div>
  );
}

function SkillConfirmationPopup({
  open,
  item,
  busy,
  onApprove,
  onReject,
  onLater,
  t,
}: {
  open: boolean;
  item: SkillConfirmation | null;
  busy: boolean;
  onApprove: (id: string) => void;
  onReject: (id: string) => void;
  onLater: (id: string) => void;
  t: ReturnType<typeof useT>;
}) {
  if (!open || !item) return null;
  return (
    <div className="modal-overlay" onClick={() => onLater(item.id)}>
      <div className="modal-content" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">{t("skillsConfirmPopupTitle")}</div>
        <div className="modal-body">
          <div>{t("skillsConfirmPopupMsg")}</div>
          <div className="json-hint" style={{ marginTop: 8 }}>{t("skillsConfirmTimeoutHint")}</div>
          <div className="skill-confirm-row" style={{ marginTop: 10 }}>
            <span className="field-label">{t("commandPreview")}</span>
            <code className="skill-command">{item.commandPreview}</code>
          </div>
          <div className="skill-confirm-row">
            <span className="field-label">{t("confirmReason")}</span>
            <span>{item.reason}</span>
          </div>
          <div className="skill-confirm-row">
            <span className="field-label">{t("createdAt")}</span>
            <span>{formatTime(item.createdAt)}</span>
          </div>
        </div>
        <div className="modal-footer">
          <button className="btn btn-secondary" disabled={busy} onClick={() => onLater(item.id)}>
            {t("decideLater")}
          </button>
          <button className="btn btn-secondary" disabled={busy} onClick={() => onReject(item.id)}>
            {t("reject")}
          </button>
          <button className="btn btn-start" disabled={busy} onClick={() => onApprove(item.id)}>
            {t("approve")}
          </button>
        </div>
      </div>
    </div>
  );
}

function SkillDirectoryListEditor({
  title,
  hint,
  items,
  onAdd,
  onRemove,
  onPathChange,
  onValidate,
  onBrowse,
  onToggleEnabled,
  enableToggle = false,
  showValidation = true,
  t,
}: {
  title: string;
  hint: string;
  items: SkillDirectoryItem[];
  onAdd: () => void;
  onRemove: (id: string) => void;
  onPathChange: (id: string, value: string) => void;
  onValidate?: (id: string) => void;
  onBrowse: (id: string) => void;
  onToggleEnabled?: (id: string) => void;
  enableToggle?: boolean;
  showValidation?: boolean;
  t: ReturnType<typeof useT>;
}) {
  const statusLabel = (status: SkillDirStatus): string => {
    if (status === "checking") return t("skillDirChecking");
    if (status === "valid") return t("skillDirValid");
    if (status === "invalid") return t("skillDirInvalid");
    if (status === "error") return t("skillDirError");
    return t("skillDirIdle");
  };

  return (
    <div className="skills-dir-panel">
      <div className="skills-dir-panel-head">
        <label className="field-label">{title}</label>
        <button className="btn-add-dir" title={t("addFolderPath")} onClick={onAdd}>+</button>
      </div>
      <div className="skills-dir-list">
        {items.map((item) => (
          <div className={`skills-dir-row ${showValidation ? "" : "no-validation"} ${enableToggle ? "with-toggle" : ""}`} key={item.id}>
            {enableToggle && (
              <button
                className={`toggle-btn skills-dir-toggle ${item.enabled ? "toggle-on" : "toggle-off"}`}
                disabled={item.status !== "valid"}
                onClick={() => onToggleEnabled?.(item.id)}
                title={item.status === "valid"
                  ? (item.enabled ? t("enabledClick") : t("disabledClick"))
                  : t("skillRootEnableBlocked")}
                aria-label={item.enabled ? t("enabledClick") : t("disabledClick")}
              />
            )}
            <input
              className="form-input skills-dir-input"
              value={item.path}
              placeholder={t("folderPathPlaceholder")}
              onChange={(e) => onPathChange(item.id, e.target.value)}
              onBlur={() => {
                if (showValidation && onValidate) {
                  onValidate(item.id);
                }
              }}
            />
            <button className="btn btn-secondary btn-sm skills-dir-browse" onClick={() => onBrowse(item.id)}>
              <FolderOpen size={13} />
              {t("browseFolder")}
            </button>
            {showValidation && (
              <>
                <span className={`skills-dir-dot ${item.status}`} aria-hidden />
                <span className={`skills-dir-status ${item.status}`}>{statusLabel(item.status)}</span>
              </>
            )}
            <button className="btn-icon btn-danger-icon skills-dir-remove" title={t("remove")} onClick={() => onRemove(item.id)}>
              ✕
            </button>
          </div>
        ))}
      </div>
      <span className="json-hint">{hint}</span>
    </div>
  );
}

// ── 单条 Server 可视化编辑行 ──────────────────────────────────────
function ServerRow({
  server,
  onChange,
  onDelete,
  running,
  baseUrl,
  ssePath,
  httpPath,
  copied,
  onCopy,
  testState,
  onTest,
  t,
}: {
  server: ServerConfig;
  onChange: (u: ServerConfig) => void;
  onDelete: () => void;
  running: boolean;
  baseUrl: string;
  ssePath: string;
  httpPath: string;
  copied: string | null;
  onCopy: (name: string, type: EndpointTransportType, url: string, key: string) => void;
  testState: ServerTestState;
  onTest: () => void;
  t: ReturnType<typeof useT>;
}) {
  const sseUrl  = `${baseUrl}${ssePath}/${server.name}`;
  const httpUrl = `${baseUrl}${httpPath}/${server.name}`;
  const showLinks = running && server.enabled && server.name.trim();
  const isTesting = testState.status === "testing";
  const statusText = testState.status === "testing"
    ? t("serverTestTesting")
    : testState.status === "success"
      ? t("serverTestSuccess")
      : testState.status === "failed"
        ? t("serverTestFailed")
        : t("serverTestIdle");
  const statusTitle = testState.testedAt
    ? `${statusText} · ${formatTime(testState.testedAt)}${testState.message ? `\n${testState.message}` : ""}`
    : (testState.message || statusText);

  // 环境变量数组形式（方便渲染）
  const envEntries = Object.entries(server.env);
  const [visibleEnvValues, setVisibleEnvValues] = useState<Record<string, boolean>>({});

  const toggleEnvValueVisibility = (rowId: string) => {
    setVisibleEnvValues((prev) => ({ ...prev, [rowId]: !prev[rowId] }));
  };

  // 添加新的环境变量 KV 对
  const addEnvVar = () => {
    onChange({ ...server, env: { ...server.env, "": "" } });
  };

  // 更新环境变量
  const updateEnvVar = (oldKey: string, newKey: string, newValue: string) => {
    const newEnv: Record<string, string> = {};
    Object.entries(server.env).forEach(([k, v]) => {
      if (k === oldKey) {
        if (newKey.trim()) {
          newEnv[newKey] = newValue;
        }
      } else {
        newEnv[k] = v;
      }
    });
    // 如果是新添加的空键
    if (oldKey === "" && newKey.trim()) {
      newEnv[newKey] = newValue;
    } else if (oldKey === "" && !newKey.trim()) {
      newEnv[""] = newValue;
    }
    onChange({ ...server, env: newEnv });
  };

  // 删除环境变量
  const removeEnvVar = (key: string) => {
    const newEnv = { ...server.env };
    delete newEnv[key];
    onChange({ ...server, env: newEnv });
  };

  return (
    <div className="server-row-wrap">
      {/* ── 服务器基本信息行 ── */}
      <div className={`server-row ${!server.enabled ? "server-row-disabled" : ""}`}>
        {/* ── 修复后的纯 CSS 滑动开关，无文字内容 ── */}
        <button
          className={`toggle-btn ${server.enabled ? "toggle-on" : "toggle-off"}`}
          title={server.enabled ? t("enabledClick") : t("disabledClick")}
          onClick={() => onChange({ ...server, enabled: !server.enabled })}
          aria-label={server.enabled ? t("enabledClick") : t("disabledClick")}
        />
        <div className="server-row-fields">
          <input className="form-input" placeholder={t("name")}
            value={server.name}
            onChange={(e) => onChange({ ...server, name: e.target.value })} />
          <input className="form-input" placeholder="npx"
            value={server.command}
            onChange={(e) => onChange({ ...server, command: e.target.value })} />
          <input className="form-input" placeholder="-y @modelcontextprotocol/server-filesystem /path"
            value={argsToStr(server.args)}
            onChange={(e) => onChange({ ...server, args: strToArgs(e.target.value) })} />
        </div>
        <span className={`server-test-chip ${testState.status}`} title={statusTitle}>{statusText}</span>
        <button
          className="btn btn-secondary btn-sm btn-test-server"
          title={t("testServerHint")}
          disabled={isTesting}
          onClick={onTest}
        >
          {isTesting ? t("serverTestTesting") : t("testServer")}
        </button>
        {/* ── 添加环境变量的加号按钮 ── */}
        <button className="btn-icon btn-add-env" title={t("addEnvVar")} onClick={addEnvVar}>+</button>
        <button className="btn-icon btn-danger-icon" title={t("remove")} onClick={onDelete}>✕</button>
      </div>

      {/* ── 环境变量 KV 对列表（仅当有环境变量时显示）── */}
      {envEntries.length > 0 && (
        <div className={`server-env-row ${!server.enabled ? "server-row-disabled" : ""}`}>
          <span className="env-label">{t("envVars")}</span>
          <div className="env-kv-list">
            {envEntries.map(([key, value], idx) => {
              const rowId = `${idx}:${key}`;
              const isVisible = visibleEnvValues[rowId] === true;
              return (
                <div className="env-kv-item" key={idx}>
                  <input
                    className="form-input env-key-input"
                    placeholder="KEY"
                    value={key}
                    onChange={(e) => updateEnvVar(key, e.target.value, value)}
                  />
                  <span className="env-kv-sep">=</span>
                  <div className="env-value-wrap">
                    <input
                      className="form-input env-value-input"
                      type={isVisible ? "text" : "password"}
                      autoComplete="off"
                      placeholder="VALUE"
                      value={value}
                      onChange={(e) => updateEnvVar(key, key, e.target.value)}
                    />
                    <button
                      type="button"
                      className="btn-icon btn-env-visibility"
                      title={isVisible ? t("hideEnvValue") : t("showEnvValue")}
                      aria-label={isVisible ? t("hideEnvValue") : t("showEnvValue")}
                      onClick={() => toggleEnvValueVisibility(rowId)}
                    >
                      {isVisible ? <EyeOff size={12} /> : <Eye size={12} />}
                    </button>
                  </div>
                  <button className="btn-icon btn-danger-icon btn-remove-env" title={t("removeEnvVar")} onClick={() => removeEnvVar(key)}>✕</button>
                </div>
              );
            })}
          </div>
        </div>
      )}

      {/* ── 运行时端点链接（直接放在 server-row 内部底部）── */}
      {showLinks && (
        <div className={`server-row-endpoints ${!server.enabled ? "server-row-disabled" : ""}`}>
          <div className="endpoint-item">
            <span className="endpoint-label">{t("endpointSSE")}</span>
            <code className="endpoint-url">{sseUrl}</code>
            <button className="btn-icon" title={t("copySSE")}
              onClick={() => onCopy(server.name, "sse", sseUrl, `${server.name}-sse`)}>
              {copied === `${server.name}-sse`
                ? <Check size={12} color="var(--accent-green)" />
                : <Copy size={12} />}
            </button>
          </div>
          <div className="endpoint-item">
            <span className="endpoint-label">{t("endpointHTTP")}</span>
            <code className="endpoint-url">{httpUrl}</code>
            <button className="btn-icon" title={t("copyHTTP")}
              onClick={() => onCopy(server.name, "streamable-http", httpUrl, `${server.name}-http`)}>
              {copied === `${server.name}-http`
                ? <Check size={12} color="var(--accent-green)" />
                : <Copy size={12} />}
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

// ── 主 App ────────────────────────────────────────────────────────
function App() {
  const [lang, setLang] = useState<Lang>(() =>
    (localStorage.getItem("mcp-lang") as Lang) ?? "zh"
  );
  const t = useT(lang);
  const toggleLang = () => {
    const next: Lang = lang === "zh" ? "en" : "zh";
    setLang(next);
    localStorage.setItem("mcp-lang", next);
  };

  // ── Tab 状态 ──
  const [activeTab, setActiveTab] = useState<"mcp" | "skills">("mcp");

  const [servers, setServers] = useState<ServerConfig[]>([]);
  const [listen, setListen] = useState("127.0.0.1:8765");
  const [apiPrefix, setApiPrefix] = useState("/api/v2");
  const [ssePath, setSsePath] = useState("/api/v2/sse");
  const [httpPath, setHttpPath] = useState("/api/v2/mcp");
  const [adminToken, setAdminToken] = useState("");
  const [mcpToken, setMcpToken] = useState("");
  const [defaultSkillRules, setDefaultSkillRules] = useState<SkillCommandRule[]>([]);
  const [skills, setSkills] = useState<SkillsConfig>(() => ensureSkillsConfig(undefined, []));
  const [skillRootItems, setSkillRootItems] = useState<SkillDirectoryItem[]>([]);
  const [skillWhitelistItems, setSkillWhitelistItems] = useState<SkillDirectoryItem[]>([]);
  const [skillsRulesDraft, setSkillsRulesDraft] = useState("[]");
  const [skillsRulesError, setSkillsRulesError] = useState<string | null>(null);
  const [skillPending, setSkillPending] = useState<SkillConfirmation[]>([]);
  const [activeSkillPopupId, setActiveSkillPopupId] = useState<string | null>(null);
  const [skillActionBusy, setSkillActionBusy] = useState<Set<string>>(new Set());
  const [status, setStatus] = useState<GatewayProcessStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [autoTestingServers, setAutoTestingServers] = useState(false);
  const [serverTestStates, setServerTestStates] = useState<Record<string, ServerTestState>>({});
  const [configLoaded, setConfigLoaded] = useState(false);
  const [serversMode, setServersMode] = useState<"visual" | "json">("visual");
  const [jsonText, setJsonText] = useState("{}");
  const [jsonError, setJsonError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [savedConfigFingerprint, setSavedConfigFingerprint] = useState("");
  const [configPath, setConfigPath] = useState<string>("");
  // 删除确认弹窗状态
  const [deleteConfirm, setDeleteConfirm] = useState<{ open: boolean; index: number; name: string }>({
    open: false, index: -1, name: ""
  });
  const [skillDirDeleteConfirm, setSkillDirDeleteConfirm] = useState<{
    open: boolean;
    kind: SkillDirKind;
    id: string;
  }>({
    open: false, kind: "roots", id: ""
  });
  const knownPendingIdsRef = useRef<Set<string>>(new Set());
  const dismissedPopupIdsRef = useRef<Set<string>>(new Set());
  const pendingFetchSeqRef = useRef(0);
  const apiClient = useMemo(
    () => new ApiClient(listen, adminToken, apiPrefix),
    [adminToken, apiPrefix, listen],
  );

  const syncSkillRootsToConfig = useCallback((nextItems: SkillDirectoryItem[]) => {
    const normalizedEntries = nextItems
      .map((item) => ({
        path: item.path.trim(),
        enabled: item.enabled,
        status: item.status,
      }))
      .filter((item) => item.path.length > 0);

    setSkills((prev) => ({
      ...prev,
      roots: normalizedEntries
        .filter((item) => item.enabled && item.status === "valid")
        .map((item) => item.path),
      rootEntries: normalizedEntries.map((item) => ({
        path: item.path,
        enabled: item.enabled,
      })),
    }));
  }, []);

  const setRootItemsAndSync = useCallback((nextItems: SkillDirectoryItem[]) => {
    setSkillRootItems(nextItems);
    syncSkillRootsToConfig(nextItems);
  }, [syncSkillRootsToConfig]);

  const setWhitelistItemsAndSync = useCallback((nextItems: SkillDirectoryItem[]) => {
    setSkillWhitelistItems(nextItems);
    setSkills((prev) => ({
      ...prev,
      policy: {
        ...prev.policy,
        pathGuard: {
        ...prev.policy.pathGuard,
          whitelistDirs: nextItems.map((item) => item.path.trim()).filter((item) => item.length > 0),
        },
      },
    }));
  }, []);

  const updateItemStatus = useCallback((kind: "roots" | "whitelist", id: string, pathSnapshot: string, status: SkillDirStatus) => {
    if (kind === "roots") {
      setSkillRootItems((prev) => {
        const next = prev.map((item) => {
          if (item.id !== id) return item;
          if (item.path.trim() !== pathSnapshot) return item;
          return {
            ...item,
            status,
            enabled: status === "valid" || status === "checking" ? item.enabled : false,
          };
        });
        syncSkillRootsToConfig(next);
        return next;
      });
      return;
    }

    setSkillWhitelistItems((prev) => prev.map((item) => {
      if (item.id !== id) return item;
      if (item.path.trim() !== pathSnapshot) return item;
      return { ...item, status };
    }));
  }, [syncSkillRootsToConfig]);

  const runItemValidation = useCallback(async (kind: "roots" | "whitelist", id: string, path: string) => {
    const normalized = path.trim();
    if (normalized.length === 0) {
      updateItemStatus(kind, id, normalized, "idle");
      return;
    }

    try {
      const result = await validateSkillDirectory(normalized);
      updateItemStatus(kind, id, normalized, skillDirectoryStatusFromResult(result));
    } catch {
      updateItemStatus(kind, id, normalized, "error");
    }
  }, [updateItemStatus]);

  const triggerItemValidation = useCallback((kind: "roots" | "whitelist", id: string, path: string) => {
    const normalized = path.trim();
    if (normalized.length === 0) {
      updateItemStatus(kind, id, normalized, "idle");
      return;
    }
    updateItemStatus(kind, id, normalized, "checking");
    void runItemValidation(kind, id, normalized);
  }, [runItemValidation, updateItemStatus]);

  // ── 初始加载配置 ──
  useEffect(() => {
    Promise.all([loadLocalConfig(), getDefaultSkillRules().catch(() => [])]).then(([cfg, builtinRules]) => {
      setDefaultSkillRules(builtinRules);
      const nextServers = cfg.servers ?? [];
      const nextListen = cfg.listen || "127.0.0.1:8765";
      const nextApiPrefix = cfg.apiPrefix || "/api/v2";
      const nextSsePath = cfg.transport?.sse?.basePath || "/api/v2/sse";
      const nextHttpPath = cfg.transport?.streamableHttp?.basePath || "/api/v2/mcp";
      const nextAdminToken = cfg.security?.admin?.token ?? "";
      const nextMcpToken = cfg.security?.mcp?.token ?? "";

      setServers(nextServers);
      setServerTestStates({});
      setAutoTestingServers(false);
      setListen(nextListen);
      setApiPrefix(nextApiPrefix);
      setSsePath(nextSsePath);
      setHttpPath(nextHttpPath);
      // 加载 Security 配置
      setAdminToken(nextAdminToken);
      setMcpToken(nextMcpToken);
      const nextSkills = ensureSkillsConfig(cfg.skills, builtinRules);
      setSkills(nextSkills);
      const rootEntries = Array.isArray(nextSkills.rootEntries) && nextSkills.rootEntries.length > 0
        ? nextSkills.rootEntries
        : nextSkills.roots.map((path) => ({ path, enabled: true }));
      const nextRootItems = (rootEntries.length > 0 ? rootEntries : [{ path: "", enabled: false }]).map((entry) => ({
        ...createSkillDirectoryItem(entry.path, entry.enabled),
        status: entry.path.trim().length > 0 ? "checking" as const : "idle" as const,
      }));
      const nextWhitelistItems = (nextSkills.policy.pathGuard.whitelistDirs.length > 0
        ? nextSkills.policy.pathGuard.whitelistDirs
        : [""]
      ).map((path) => ({
        ...createSkillDirectoryItem(path, true),
        status: "idle" as const,
      }));
      setSkillRootItems(nextRootItems);
      setSkillWhitelistItems(nextWhitelistItems);
      nextRootItems.forEach((item) => {
        if (item.path.trim().length > 0) {
          void runItemValidation("roots", item.id, item.path);
        }
      });
      setSkillsRulesDraft(JSON.stringify(nextSkills.policy.rules, null, 2));
      setSkillsRulesError(null);
      setJsonText(JSON.stringify(serversToJson(nextServers), null, 2));
      setSavedConfigFingerprint(createEditableConfigFingerprint(createEditableConfigSnapshot({
        servers: nextServers,
        listen: nextListen,
        apiPrefix: nextApiPrefix,
        ssePath: nextSsePath,
        httpPath: nextHttpPath,
        adminToken: nextAdminToken,
        mcpToken: nextMcpToken,
        skills: nextSkills,
      })));
      setConfigLoaded(true);
    }).catch((e) => setError(String(e)));
    // 获取配置文件路径
    getConfigPath().then(setConfigPath).catch(() => {});
  }, [runItemValidation]);

  const parsedJsonServers = useMemo(() => {
    if (serversMode !== "json") {
      return null;
    }
    try {
      return jsonToServers(JSON.parse(jsonText) as Record<string, unknown>);
    } catch {
      return null;
    }
  }, [jsonText, serversMode]);

  const switchToJson = () => {
    setJsonText(JSON.stringify(serversToJson(servers), null, 2));
    setJsonError(null);
    setServersMode("json");
  };
  const switchToVisual = () => {
    if (!parsedJsonServers) {
      setJsonError(t("jsonParseError"));
      return;
    }
    setServers(parsedJsonServers);
    setServerTestStates({});
    setJsonError(null);
    setServersMode("visual");
  };

  // ── 进程状态轮询 ──
  const refreshStatus = useCallback(async (): Promise<PollOutcome> => {
    try {
      const current = await getGatewayStatus();
      setStatus(current);
      return current.running ? "active" : "idle";
    } catch {
      return "error";
    }
  }, []);
  usePolling(refreshStatus, {
    enabled: true,
    activeMs: 3000,
    idleMs: 10000,
    errorMs: 5000,
    immediate: true,
  });

  const running = !!status?.running;

  const runServerConnectivityTest = useCallback(async (server: ServerConfig, index: number) => {
    const key = serverTestKey(index, server);
    if (!server.command.trim()) {
      setServerTestStates((prev) => ({
        ...prev,
        [key]: {
          status: "failed",
          message: t("serverTestMissingCommand"),
        },
      }));
      return;
    }

    setServerTestStates((prev) => ({
      ...prev,
      [key]: {
        status: "testing",
        message: "",
      },
    }));

    try {
      const result: ServerConnectivityTestResult = await testMcpServerLocal(server);
      setServerTestStates((prev) => ({
        ...prev,
        [key]: {
          status: result.ok ? "success" : "failed",
          message: "",
          testedAt: typeof result.testedAt === "string" ? result.testedAt : new Date().toISOString(),
        },
      }));
    } catch (error) {
      setServerTestStates((prev) => ({
        ...prev,
        [key]: {
          status: "failed",
          message: asErrorMessage(error),
        },
      }));
    }
  }, [t]);

  const runEnabledServerConnectivityTests = useCallback(async (targetServers: ServerConfig[]) => {
    const candidates = targetServers
      .map((server, index) => ({ server, index }))
      .filter(({ server }) => server.enabled && server.command.trim().length > 0);
    if (candidates.length === 0) {
      return;
    }

    setAutoTestingServers(true);
    try {
      for (const item of candidates) {
        await runServerConnectivityTest(item.server, item.index);
      }
    } finally {
      setAutoTestingServers(false);
    }
  }, [runServerConnectivityTest]);

  const fetchSkillPending = useCallback(async (): Promise<PollOutcome> => {
    const fetchSeq = pendingFetchSeqRef.current + 1;
    pendingFetchSeqRef.current = fetchSeq;

    if (!running || !skills.enabled) {
      if (fetchSeq !== pendingFetchSeqRef.current) return "idle";
      setSkillPending([]);
      setActiveSkillPopupId(null);
      knownPendingIdsRef.current = new Set();
      dismissedPopupIdsRef.current = new Set();
      return "idle";
    }
    try {
      const pending = await apiClient.listSkillConfirmations();
      if (fetchSeq !== pendingFetchSeqRef.current) return "idle";
      const nextIds = new Set(pending.map((item) => item.id));
      const knownIds = knownPendingIdsRef.current;
      const newItems = pending.filter((item) => !knownIds.has(item.id));

      dismissedPopupIdsRef.current = new Set(
        Array.from(dismissedPopupIdsRef.current).filter((id) => nextIds.has(id)),
      );

      if (newItems.length > 0) {
        setActiveTab("skills");
        void focusMainWindowForSkillConfirmation().catch(() => {});
        const popupCandidate = newItems.find((item) => !dismissedPopupIdsRef.current.has(item.id)) ?? newItems[0];
        setActiveSkillPopupId(popupCandidate.id);
      }

      setSkillPending(pending);
      setActiveSkillPopupId((current) => {
        if (current && nextIds.has(current)) return current;
        const fallback = pending.find((item) => !dismissedPopupIdsRef.current.has(item.id));
        return fallback ? fallback.id : null;
      });
      knownPendingIdsRef.current = nextIds;
      return pending.length > 0 ? "active" : "idle";
    } catch (error) {
      if (fetchSeq !== pendingFetchSeqRef.current) return "idle";
      setError(String(error));
      return "error";
    }
  }, [apiClient, running, skills.enabled]);

  usePolling(fetchSkillPending, {
    enabled: running && skills.enabled,
    activeMs: 3000,
    idleMs: 10000,
    errorMs: 5000,
    immediate: true,
  });

  const updateConfirmation = useCallback(async (id: string, action: "approve" | "reject") => {
    setSkillActionBusy((prev) => {
      const next = new Set(prev);
      next.add(id);
      return next;
    });
    let success = false;
    try {
      if (action === "approve") {
        await apiClient.approveSkillConfirmation(id);
      } else {
        await apiClient.rejectSkillConfirmation(id);
      }
      await fetchSkillPending();
      success = true;
    } catch (error) {
      if (isConfirmationAlreadyResolvedError(error)) {
        await fetchSkillPending();
        success = true;
      } else {
        setError(String(error));
      }
    } finally {
      setSkillActionBusy((prev) => {
        const next = new Set(prev);
        next.delete(id);
        return next;
      });
    }
    return success;
  }, [apiClient, fetchSkillPending]);

  const handleSkillConfirmationAction = useCallback((id: string, action: "approve" | "reject") => {
    void (async () => {
      const ok = await updateConfirmation(id, action);
      if (!ok) return;
      dismissedPopupIdsRef.current = new Set(dismissedPopupIdsRef.current).add(id);
      setActiveSkillPopupId((current) => (current === id ? null : current));
    })();
  }, [updateConfirmation]);

  const deferSkillConfirmationPopup = useCallback((id: string) => {
    dismissedPopupIdsRef.current = new Set(dismissedPopupIdsRef.current).add(id);
    setActiveSkillPopupId((current) => (current === id ? null : current));
  }, []);

  const activeSkillPopupItem = useMemo(
    () => skillPending.find((item) => item.id === activeSkillPopupId) ?? null,
    [activeSkillPopupId, skillPending],
  );

  const onRulesDraftChange = (value: string) => {
    setSkillsRulesDraft(value);
    try {
      const parsed = parseRulesJson(value);
      setSkills((prev) => ({
        ...prev,
        policy: {
          ...prev.policy,
          rules: parsed,
        },
      }));
      setSkillsRulesError(null);
    } catch {
      setSkillsRulesError(t("skillsRulesJsonError"));
    }
  };

  const addRootItem = () => {
    setRootItemsAndSync([...skillRootItems, createSkillDirectoryItem("", false)]);
  };

  const addWhitelistItem = () => {
    setWhitelistItemsAndSync([...skillWhitelistItems, createSkillDirectoryItem("", true)]);
  };

  const removeRootItem = (id: string) => {
    const next = skillRootItems.filter((item) => item.id !== id);
    setRootItemsAndSync(next.length > 0 ? next : [createSkillDirectoryItem("", false)]);
  };

  const removeWhitelistItem = (id: string) => {
    const next = skillWhitelistItems.filter((item) => item.id !== id);
    setWhitelistItemsAndSync(next.length > 0 ? next : [createSkillDirectoryItem("", true)]);
  };

  const requestRemoveRootItem = (id: string) => {
    setSkillDirDeleteConfirm({ open: true, kind: "roots", id });
  };

  const requestRemoveWhitelistItem = (id: string) => {
    setSkillDirDeleteConfirm({ open: true, kind: "whitelist", id });
  };

  const confirmSkillDirDelete = () => {
    const { kind, id } = skillDirDeleteConfirm;
    if (!id) {
      setSkillDirDeleteConfirm({ open: false, kind: "roots", id: "" });
      return;
    }

    if (kind === "roots") {
      removeRootItem(id);
    } else {
      removeWhitelistItem(id);
    }
    setSkillDirDeleteConfirm({ open: false, kind: "roots", id: "" });
  };

  const cancelSkillDirDelete = () => {
    setSkillDirDeleteConfirm({ open: false, kind: "roots", id: "" });
  };

  const updateRootItemPath = (id: string, path: string) => {
    setRootItemsAndSync(skillRootItems.map((item) =>
      item.id === id ? { ...item, path, status: "idle", enabled: false } : item
    ));
  };

  const updateWhitelistItemPath = (id: string, path: string) => {
    setWhitelistItemsAndSync(skillWhitelistItems.map((item) =>
      item.id === id ? { ...item, path, status: "idle" } : item
    ));
  };

  const validateRootItem = (id: string) => {
    const target = skillRootItems.find((item) => item.id === id);
    if (!target) return;
    triggerItemValidation("roots", target.id, target.path);
  };

  const browseRootItem = async (id: string) => {
    const target = skillRootItems.find((item) => item.id === id);
    if (!target) return;
    try {
      const selected = await pickFolderDialog(target.path.trim() || undefined);
      if (!selected) return;
      setRootItemsAndSync(skillRootItems.map((item) =>
        item.id === id ? { ...item, path: selected, status: "checking", enabled: false } : item
      ));
      void runItemValidation("roots", id, selected);
    } catch (error) {
      setError(String(error));
    }
  };

  const toggleRootItemEnabled = (id: string) => {
    setRootItemsAndSync(skillRootItems.map((item) => {
      if (item.id !== id) return item;
      if (item.status !== "valid") return { ...item, enabled: false };
      return { ...item, enabled: !item.enabled };
    }));
  };

  const browseWhitelistItem = async (id: string) => {
    const target = skillWhitelistItems.find((item) => item.id === id);
    if (!target) return;
    try {
      const selected = await pickFolderDialog(target.path.trim() || undefined);
      if (!selected) return;
      setWhitelistItemsAndSync(skillWhitelistItems.map((item) =>
        item.id === id ? { ...item, path: selected, status: "idle" } : item
      ));
    } catch (error) {
      setError(String(error));
    }
  };

  const resolveServers = (): ServerConfig[] | null => {
    let list: ServerConfig[];
    if (serversMode === "json") {
      if (!parsedJsonServers) {
        setJsonError(t("jsonParseErrorStart"));
        return null;
      }
      list = parsedJsonServers;
    } else {
      list = servers;
    }
    const valid = list.filter((s) => s.name.trim() && s.command.trim());
    if (list.length > 0 && valid.length === 0) {
      setError(t("allServersInvalid"));
      return null;
    }
    return valid;
  };

  const currentConfigFingerprint = useMemo(() => {
    if (serversMode === "json" && !parsedJsonServers) {
      return null;
    }
    const compareServers = serversMode === "json" ? parsedJsonServers : servers;

    return createEditableConfigFingerprint(createEditableConfigSnapshot({
      servers: compareServers ?? servers,
      listen,
      apiPrefix,
      ssePath,
      httpPath,
      adminToken,
      mcpToken,
      skills: ensureSkillsConfig(skills, defaultSkillRules),
    }));
  }, [servers, serversMode, parsedJsonServers, listen, apiPrefix, ssePath, httpPath, adminToken, mcpToken, skills, defaultSkillRules]);

  const isConfigDirty = configLoaded
    && (currentConfigFingerprint === null || currentConfigFingerprint !== savedConfigFingerprint);
  const showRestartRequiredHint = running && isConfigDirty;

  const persistConfig = async (nextServers: ServerConfig[]) => {
    const snapshot = createEditableConfigSnapshot({
      servers: nextServers,
      listen,
      apiPrefix,
      ssePath,
      httpPath,
      adminToken,
      mcpToken,
      skills: ensureSkillsConfig(skills, defaultSkillRules),
    });
    const cfg: GatewayConfig = await loadLocalConfig();
    cfg.servers = snapshot.servers;
    cfg.listen = snapshot.listen;
    cfg.apiPrefix = snapshot.apiPrefix;
    cfg.transport = snapshot.transport;
    cfg.security = snapshot.security;
    cfg.skills = snapshot.skills;
    await saveLocalConfig(cfg);
    return createEditableConfigFingerprint(snapshot);
  };

  const handleStart = async () => {
    if (skillsRulesError) {
      setError(skillsRulesError);
      return;
    }
    const nextServers = resolveServers();
    if (nextServers === null) return;
    setError(null); setBusy(true);
    try {
      const latestFingerprint = await persistConfig(nextServers);
      setSavedConfigFingerprint(latestFingerprint);
      await startGateway();
      await refreshStatus();
      await fetchSkillPending();
      void runEnabledServerConnectivityTests(nextServers);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  const handleStop = async () => {
    setError(null); setBusy(true);
    try {
      await stopGateway();
      await refreshStatus();
      setSkillPending([]);
      setAutoTestingServers(false);
    }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  // ── 独立保存配置 ──
  const handleSave = async () => {
    if (skillsRulesError) {
      setError(skillsRulesError);
      return;
    }
    const nextServers = resolveServers();
    if (nextServers === null) return;
    setError(null); setSaving(true); setSaveSuccess(false);
    try {
      const latestFingerprint = await persistConfig(nextServers);
      setSavedConfigFingerprint(latestFingerprint);
      setSaveSuccess(true);
      setTimeout(() => setSaveSuccess(false), 2000);
    } catch (e) { setError(String(e)); }
    finally { setSaving(false); }
  };

  // ── 删除确认逻辑 ──
  const requestDelete = (index: number, name: string) => {
    setDeleteConfirm({ open: true, index, name: name || `服务 ${index + 1}` });
  };
  const confirmDelete = () => {
    setServers((prev) => prev.filter((_, xi) => xi !== deleteConfirm.index));
    setServerTestStates({});
    setDeleteConfirm({ open: false, index: -1, name: "" });
  };
  const cancelDelete = () => {
    setDeleteConfirm({ open: false, index: -1, name: "" });
  };

  const baseUrl = listen.startsWith("http") ? listen : `http://${listen}`;
  const skillHttpUrl = `${baseUrl}${httpPath}/${skills.serverName}`;
  const skillSseUrl = `${baseUrl}${ssePath}/${skills.serverName}`;

  const handleCopy = async (name: string, type: EndpointTransportType, url: string, key: string) => {
    const snippet = createMcpClientEntryJson(name, type, url);
    await navigator.clipboard.writeText(snippet);
    setCopied(key);
    setTimeout(() => setCopied(null), 2000);
  };

  const handleOpenExternalLink = useCallback(async (url: string) => {
    try {
      await open(url);
    } catch (error) {
      setError(String(error));
    }
  }, []);

  const handleOpenQqGroup = useCallback(async () => {
    const inviteUrl = QQ_GROUP_INVITE_URL.trim();
    if (inviteUrl.length > 0) {
      try {
        await open(inviteUrl);
      } catch (error) {
        setError(String(error));
      }
      return;
    }

    for (const link of QQ_GROUP_PROTOCOL_LINKS) {
      try {
        await open(link);
        return;
      } catch {
        // keep trying next known protocol.
      }
    }
    setError(t("qqGroupFallbackHint").replace("{group}", QQ_GROUP_NUMBER));
  }, [t]);

  return (
    <div className="app-root">

      {/* ── 顶栏 ── */}
      <div className="topbar">
        <div className="topbar-left">
          <span className={`status-dot ${running ? "running" : "stopped"}`} />
          <span className="topbar-title">{t("appTitle")}</span>
          <span className="topbar-subtitle">{running ? t("running") : t("stopped")}</span>
        </div>
        <div className="topbar-right">
          {showRestartRequiredHint && (
            <span
              className="topbar-restart-hint"
              role="status"
              aria-live="polite"
              title={t("restartRequiredHint")}
            >
              {t("restartRequiredHint")}
            </span>
          )}
          <button
            className={`btn btn-secondary btn-sm ${isConfigDirty ? "btn-save-dirty" : ""}`}
            onClick={handleSave}
            disabled={saving || !configLoaded}
            title={isConfigDirty ? t("saveConfigUnsaved") : t("saveConfig")}
          >
            <Save size={13} />
            <span>{saving ? t("saving") : saveSuccess ? t("saveSuccess") : t("saveConfig")}</span>
            {isConfigDirty && !saving && (
              <span
                className="save-dirty-indicator"
                aria-label={t("saveConfigUnsaved")}
                title={t("saveConfigUnsaved")}
              >
                !
              </span>
            )}
          </button>
          <button className="btn-lang" onClick={toggleLang} title="Switch language">
            <Languages size={13} />
            <span>{t("langToggle")}</span>
          </button>
          {!running ? (
            <button className="btn btn-start" onClick={handleStart} disabled={busy || !configLoaded}>
              <Play size={14} />{busy ? t("starting") : t("start")}
            </button>
          ) : (
            <button className="btn btn-stop" onClick={handleStop} disabled={busy}>
              <Square size={14} />{busy ? t("stopping") : t("stop")}
            </button>
          )}
        </div>
      </div>

      {/* ── Tab 导航 ── */}
      <div className="tab-nav">
        <button
          className={`tab-button ${activeTab === "mcp" ? "active" : ""}`}
          onClick={() => setActiveTab("mcp")}
        >
          {t("tabMcp")}
        </button>
        <button
          className={`tab-button ${activeTab === "skills" ? "active" : ""}`}
          onClick={() => setActiveTab("skills")}
        >
          {t("tabSkills")}
          {skillPending.length > 0 && (
            <span className="tab-badge">{skillPending.length}</span>
          )}
        </button>
      </div>

      {/* ── 错误提示 ── */}
      {error && (
        <div className="alert alert-error" style={{ margin: "10px 20px 0" }}>
          {error}
          <button className="alert-close" onClick={() => setError(null)}>✕</button>
        </div>
      )}

      <div className="main-scroll">

        {/* ════════════════ MCP Tab ════════════════ */}
        {activeTab === "mcp" && (
          <>
            {/* ── 网关设置 ── */}
            <section className="config-section">
              <div className="section-heading">{t("gatewaySettings")}</div>
              <div className="gateway-fields">
                <div className="gw-field">
                  <label className="field-label">{t("listenAddress")}</label>
                  <input className="form-input" placeholder="127.0.0.1:8765"
                    value={listen} onChange={(e) => setListen(e.target.value)} />
                </div>
                <div className="gw-field">
                  <label className="field-label">{t("ssePath")}</label>
                  <input className="form-input" placeholder="/api/v2/sse"
                    value={ssePath} onChange={(e) => setSsePath(e.target.value)} />
                </div>
                <div className="gw-field">
                  <label className="field-label">{t("httpStreamPath")}</label>
                  <input className="form-input" placeholder="/api/v2/mcp"
                    value={httpPath} onChange={(e) => setHttpPath(e.target.value)} />
                </div>
              </div>
            </section>

            {/* ── 安全配置 ── */}
            <section className="config-section">
              <div className="section-heading">{t("securityConfig")}</div>
              <div className="security-fields">
                <div className="gw-field">
                  <label className="field-label">{t("adminToken")}</label>
                  <input className="form-input" type="password" placeholder={t("tokenPlaceholder")}
                    value={adminToken} onChange={(e) => setAdminToken(e.target.value)} />
                </div>
                <div className="gw-field">
                  <label className="field-label">{t("mcpToken")}</label>
                  <input className="form-input" type="password" placeholder={t("tokenPlaceholder")}
                    value={mcpToken} onChange={(e) => setMcpToken(e.target.value)} />
                </div>
              </div>
            </section>

            {/* ── MCP Servers ── */}
            <section className="config-section">
              <div className="section-heading-row">
                <span className="section-heading" style={{ marginBottom: 0 }}>{t("mcpServers")}</span>
                <div className="section-heading-actions">
                  <div className="mode-toggle">
                    <button className={`mode-btn ${serversMode === "visual" ? "active" : ""}`}
                      onClick={switchToVisual} title={t("visual")}>
                      <List size={13} /> {t("visual")}
                    </button>
                    <button className={`mode-btn ${serversMode === "json" ? "active" : ""}`}
                      onClick={switchToJson} title={t("json")}>
                      <Code2 size={13} /> {t("json")}
                    </button>
                  </div>
                </div>
              </div>

              {jsonError && (
                <div className="alert alert-error" style={{ marginBottom: 10 }}>{jsonError}</div>
              )}
              {autoTestingServers && (
                <div className="json-hint" style={{ marginBottom: 10 }}>{t("autoTestingServers")}</div>
              )}

              {serversMode === "visual" ? (
                <>
                  {servers.length === 0 ? (
                    <div className="empty-hint">{t("noServers")}</div>
                  ) : (
                    <div className="servers-list">
                      {servers.map((s, i) => (
                        <div className="server-block" key={i}>
                          <div className="server-row-header">
                            <span className="col-toggle" />
                            <span>{t("name")}</span>
                            <span>{t("command")}</span>
                            <span>{t("args")}</span>
                            <span />
                          </div>
                          <ServerRow server={s}
                            running={running}
                            baseUrl={baseUrl}
                            ssePath={ssePath}
                            httpPath={httpPath}
                            copied={copied}
                            onCopy={handleCopy}
                            testState={serverTestStates[serverTestKey(i, s)] ?? { status: "idle", message: "" }}
                            onTest={() => { void runServerConnectivityTest(s, i); }}
                            t={t}
                            onChange={(u) => {
                              setServers((prev) => prev.map((x, xi) => xi === i ? u : x));
                              setServerTestStates((prev) => {
                                const next = { ...prev };
                                delete next[serverTestKey(i, s)];
                                delete next[serverTestKey(i, u)];
                                return next;
                              });
                            }}
                            onDelete={() => requestDelete(i, s.name)}
                          />
                        </div>
                      ))}
                    </div>
                  )}
                  <button className="btn btn-secondary btn-sm" style={{ marginTop: 10 }}
                    onClick={() => {
                      setServers((prev) => [...prev, {
                        name: "", command: "npx", args: ["-y", ""],
                        description: "", cwd: "", env: {}, lifecycle: null, stdioProtocol: "auto", enabled: true,
                      }]);
                      setServerTestStates({});
                    }}>
                    {t("addServer")}
                  </button>
                </>
              ) : (
                <div className="json-editor-wrap">
                  <div className="json-hint">{t("jsonHint")}</div>
                  <JsonEditor
                    value={jsonText}
                    onChange={(v) => { setJsonText(v); setJsonError(null); }}
                    placeholder={t("jsonHint")}
                    onFormatError={(msg) => setJsonError(msg)}
                    formatBtnText={t("formatJson")}
                  />
                </div>
              )}
            </section>
          </>
        )}

        {/* ════════════════ SKILLS Tab ════════════════ */}
        {activeTab === "skills" && (
          <>
            {/* ── Skills 基础配置 ── */}
            <section className="config-section skills-redesign-section">
              <div className="section-heading">{t("skillsConfig")}</div>

              <div className="skills-redesign">
                <div className="skills-top-row">
                  <div className="skills-switch-card">
                    <label className="field-label" htmlFor="skills-enabled-toggle">{t("skillsEnable")}</label>
                    <div className="skills-switch-track">
                      <button
                        id="skills-enabled-toggle"
                        className={`toggle-btn ${skills.enabled ? "toggle-on" : "toggle-off"}`}
                        onClick={() => setSkills((prev) => ({ ...prev, enabled: !prev.enabled }))}
                        aria-label={t("skillsEnable")}
                        title={t("skillsEnable")}
                      />
                    </div>
                  </div>
                  <div className="skills-input-card">
                    <label className="field-label" htmlFor="skills-server-name">{t("skillsServerName")}</label>
                    <input
                      id="skills-server-name"
                      className="form-input"
                      value={skills.serverName}
                      onChange={(e) => setSkills((prev) => ({ ...prev, serverName: e.target.value }))}
                      placeholder="__skills__"
                    />
                  </div>
                </div>

                <SkillDirectoryListEditor
                  title={t("skillsRoots")}
                  hint={t("skillsRootsHint")}
                  items={skillRootItems}
                  onAdd={addRootItem}
                  onRemove={requestRemoveRootItem}
                  onPathChange={updateRootItemPath}
                  onValidate={validateRootItem}
                  onBrowse={browseRootItem}
                  onToggleEnabled={toggleRootItemEnabled}
                  enableToggle
                  t={t}
                />
                {running && skills.enabled && skills.serverName.trim() && (
                  <div className="server-row-endpoints skills-endpoints">
                    <div className="endpoint-item">
                      <span className="endpoint-label">{t("skillSseEndpoint")}</span>
                      <code className="endpoint-url">{skillSseUrl}</code>
                      <button className="btn-icon" title={t("copySkillSse")} onClick={() => handleCopy(skills.serverName, "sse", skillSseUrl, "skills-sse")}>
                        {copied === "skills-sse" ? <Check size={12} color="var(--accent-green)" /> : <Copy size={12} />}
                      </button>
                    </div>
                    <div className="endpoint-item">
                      <span className="endpoint-label">{t("skillHttpEndpoint")}</span>
                      <code className="endpoint-url">{skillHttpUrl}</code>
                      <button className="btn-icon" title={t("copySkillHttp")} onClick={() => handleCopy(skills.serverName, "streamable-http", skillHttpUrl, "skills-http")}>
                        {copied === "skills-http" ? <Check size={12} color="var(--accent-green)" /> : <Copy size={12} />}
                      </button>
                    </div>
                  </div>
                )}
              </div>
            </section>

            {/* ── 路径守卫配置 ── */}
            <section className="config-section skills-redesign-section">
              <div className="section-heading">{t("skillsPathGuard")}</div>

              <div className="skills-redesign">
                <div className="skills-top-row">
                  <div className="skills-switch-card">
                    <label className="field-label" htmlFor="skills-pathguard-toggle">{t("skillsPathGuardEnable")}</label>
                    <div className="skills-switch-track">
                      <button
                        id="skills-pathguard-toggle"
                        className={`toggle-btn ${skills.policy.pathGuard.enabled ? "toggle-on" : "toggle-off"}`}
                        onClick={() =>
                          setSkills((prev) => ({
                            ...prev,
                            policy: { ...prev.policy, pathGuard: { ...prev.policy.pathGuard, enabled: !prev.policy.pathGuard.enabled } },
                          }))
                        }
                        aria-label={t("skillsPathGuardEnable")}
                        title={t("skillsPathGuardEnable")}
                      />
                    </div>
                  </div>
                  <div className="skills-input-card">
                    <label className="field-label" htmlFor="skills-violation-action">{t("skillsViolationAction")}</label>
                    <select
                      id="skills-violation-action"
                      className="form-input skills-action-select"
                      value={skills.policy.pathGuard.onViolation}
                      onChange={(e) =>
                        setSkills((prev) => ({
                          ...prev,
                          policy: {
                            ...prev.policy,
                            pathGuard: { ...prev.policy.pathGuard, onViolation: e.target.value as SkillPolicyAction },
                          },
                        }))
                      }
                    >
                      <option value="allow">{t("policyAllow")}</option>
                      <option value="confirm">{t("policyConfirm")}</option>
                      <option value="deny">{t("policyDeny")}</option>
                    </select>
                  </div>
                </div>

                <SkillDirectoryListEditor
                  title={t("skillsWhitelistDirs")}
                  hint={t("skillsWhitelistHint")}
                  items={skillWhitelistItems}
                  onAdd={addWhitelistItem}
                  onRemove={requestRemoveWhitelistItem}
                  onPathChange={updateWhitelistItemPath}
                  onBrowse={browseWhitelistItem}
                  showValidation={false}
                  t={t}
                />
              </div>
            </section>

            {/* ── 执行配置 ── */}
            <section className="config-section">
              <div className="section-heading">{t("skillsExecution")}</div>
              <div className="skills-execution-config">
                <div className="gw-field">
                  <label className="field-label">{t("skillsExecutionTimeout")}</label>
                  <input
                    type="number"
                    className="form-input"
                    value={skills.execution.timeoutMs}
                    min={1000}
                    onChange={(e) =>
                      {
                        const val = parseInt(e.target.value, 10);
                        if (!isNaN(val) && val >= 1000) {
                          setSkills((prev) => ({
                            ...prev,
                            execution: { ...prev.execution, timeoutMs: val },
                          }));
                        }
                      }
                    }
                    placeholder="30000"
                  />
                  <span className="json-hint">{t("skillsExecutionTimeoutHint")}</span>
                </div>
                <div className="gw-field">
                  <label className="field-label">{t("skillsMaxOutputBytes")}</label>
                  <input
                    type="number"
                    className="form-input"
                    value={skills.execution.maxOutputBytes}
                    min={1024}
                    onChange={(e) =>
                      {
                        const val = parseInt(e.target.value, 10);
                        if (!isNaN(val) && val >= 1024) {
                          setSkills((prev) => ({
                            ...prev,
                            execution: { ...prev.execution, maxOutputBytes: val },
                          }));
                        }
                      }
                    }
                    placeholder="13107200"
                  />
                  <span className="json-hint">{t("skillsMaxOutputBytesHint")}</span>
                </div>
              </div>
            </section>

            {/* ── 策略规则 ── */}
            <section className="config-section">
              <div className="section-heading">{t("skillsRules")}</div>
              <div className="gw-field">
                <textarea
                  className="form-textarea skills-rules-textarea"
                  value={skillsRulesDraft}
                  onChange={(e) => onRulesDraftChange(e.target.value)}
                  placeholder={t("skillsRulesHint")}
                />
                <span className="json-hint">{t("skillsRulesHint")}</span>
                {skillsRulesError && <span className="skills-rules-error">{skillsRulesError}</span>}
              </div>
            </section>

            {/* ── 待确认命令 ── */}
            <section className="config-section">
              <div className="section-heading">{t("skillsPendingTitle")}</div>
              <SkillConfirmations
                pending={skillPending}
                busyIds={skillActionBusy}
                onApprove={(id) => { handleSkillConfirmationAction(id, "approve"); }}
                onReject={(id) => { handleSkillConfirmationAction(id, "reject"); }}
                t={t}
              />
            </section>
          </>
        )}

      </div>

      {/* ── 底部通知条：配置文件位置 + 快捷入口 ── */}
      <div className="bottom-bar">
        <div className="bottom-bar-main">
          {configPath && (
            <>
              <FolderOpen size={14} />
              <span className="bottom-bar-label">{t("configPath")}:</span>
              <code className="bottom-bar-path">{configPath}</code>
            </>
          )}
        </div>
        <div className="bottom-bar-links" role="group" aria-label={t("quickLinks")}>
          <button
            type="button"
            className="bottom-link-btn"
            aria-label={t("openBlog")}
            title={t("openBlog")}
            onClick={() => { void handleOpenExternalLink(BLOG_URL); }}
          >
            <Globe size={15} />
          </button>
          <button
            type="button"
            className="bottom-link-btn"
            aria-label={t("openGithub")}
            title={t("openGithub")}
            onClick={() => { void handleOpenExternalLink(GITHUB_URL); }}
          >
            <Github size={15} />
          </button>
          <button
            type="button"
            className="bottom-link-btn"
            aria-label={t("openQqGroup")}
            title={t("openQqGroup")}
            onClick={() => { void handleOpenQqGroup(); }}
          >
            <QqLogoIcon size={14} />
          </button>
          <button
            type="button"
            className="bottom-link-btn"
            aria-label={t("openTelegramGroup")}
            title={t("openTelegramGroup")}
            onClick={() => { void handleOpenExternalLink(TG_GROUP_URL); }}
          >
            <Send size={14} />
          </button>
        </div>
      </div>

      <SkillConfirmationPopup
        open={!!activeSkillPopupItem}
        item={activeSkillPopupItem}
        busy={!!activeSkillPopupItem && skillActionBusy.has(activeSkillPopupItem.id)}
        onApprove={(id) => handleSkillConfirmationAction(id, "approve")}
        onReject={(id) => handleSkillConfirmationAction(id, "reject")}
        onLater={deferSkillConfirmationPopup}
        t={t}
      />

      {/* ── 删除确认弹窗 ── */}
      <ConfirmDialog
        open={deleteConfirm.open}
        title={t("confirmDeleteTitle")}
        message={t("confirmDeleteMsg").replace("{name}", deleteConfirm.name)}
        onCancel={cancelDelete}
        onConfirm={confirmDelete}
        t={t}
      />
      <ConfirmDialog
        open={skillDirDeleteConfirm.open}
        title={t("confirmDeleteTitle")}
        message={skillDirDeleteConfirm.kind === "roots"
          ? t("confirmDeleteSkillRootMsg")
          : t("confirmDeleteWhitelistDirMsg")}
        onCancel={cancelSkillDirDelete}
        onConfirm={confirmSkillDirDelete}
        t={t}
      />
    </div>
  );
}

export default App;
