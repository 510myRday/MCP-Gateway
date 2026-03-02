import { useState, useEffect, useCallback } from "react";
import { Play, Square, Copy, Check, Code2, List, Languages, Save, FolderOpen } from "lucide-react";
import { getGatewayStatus, startGateway, stopGateway, type GatewayProcessStatus } from "./gatewayRuntime";
import { loadLocalConfig, saveLocalConfig, getConfigPath } from "./localConfig";
import type { GatewayConfig, ServerConfig } from "./types";
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

// ── 删除确认弹窗组件 ──────────────────────────────────────────────
function ConfirmDialog({ open, serverName, onCancel, onConfirm, t }: {
  open: boolean;
  serverName: string;
  onCancel: () => void;
  onConfirm: () => void;
  t: ReturnType<typeof useT>;
}) {
  if (!open) return null;
  return (
    <div className="modal-overlay" onClick={onCancel}>
      <div className="modal-content" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">{t("confirmDeleteTitle")}</div>
        <div className="modal-body">
          {t("confirmDeleteMsg").replace("{name}", serverName)}
        </div>
        <div className="modal-footer">
          <button className="btn btn-secondary" onClick={onCancel}>{t("cancel")}</button>
          <button className="btn btn-danger" onClick={onConfirm}>{t("confirmDelete")}</button>
        </div>
      </div>
    </div>
  );
}

// ── 单条 Server 可视化编辑行 ──────────────────────────────────────
function ServerRow({ server, onChange, onDelete, running, baseUrl, ssePath, httpPath, copied, onCopy, t }: {
  server: ServerConfig;
  onChange: (u: ServerConfig) => void;
  onDelete: () => void;
  running: boolean;
  baseUrl: string;
  ssePath: string;
  httpPath: string;
  copied: string | null;
  onCopy: (url: string, key: string) => void;
  t: ReturnType<typeof useT>;
}) {
  const sseUrl  = `${baseUrl}${ssePath}/${server.name}`;
  const httpUrl = `${baseUrl}${httpPath}/${server.name}`;
  const showLinks = running && server.enabled && server.name.trim();

  // 环境变量数组形式（方便渲染）
  const envEntries = Object.entries(server.env);

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
        {/* ── 添加环境变量的加号按钮 ── */}
        <button className="btn-icon btn-add-env" title={t("addEnvVar")} onClick={addEnvVar}>+</button>
        <button className="btn-icon btn-danger-icon" title={t("remove")} onClick={onDelete}>✕</button>
      </div>

      {/* ── 环境变量 KV 对列表（仅当有环境变量时显示）── */}
      {envEntries.length > 0 && (
        <div className={`server-env-row ${!server.enabled ? "server-row-disabled" : ""}`}>
          <span className="env-label">{t("envVars")}</span>
          <div className="env-kv-list">
            {envEntries.map(([key, value], idx) => (
              <div className="env-kv-item" key={idx}>
                <input
                  className="form-input env-key-input"
                  placeholder="KEY"
                  value={key}
                  onChange={(e) => updateEnvVar(key, e.target.value, value)}
                />
                <span className="env-kv-sep">=</span>
                <input
                  className="form-input env-value-input"
                  placeholder="VALUE"
                  value={value}
                  onChange={(e) => updateEnvVar(key, key, e.target.value)}
                />
                <button className="btn-icon btn-danger-icon btn-remove-env" title={t("removeEnvVar")} onClick={() => removeEnvVar(key)}>✕</button>
              </div>
            ))}
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
              onClick={() => onCopy(sseUrl, `${server.name}-sse`)}>
              {copied === `${server.name}-sse`
                ? <Check size={12} color="var(--accent-green)" />
                : <Copy size={12} />}
            </button>
          </div>
          <div className="endpoint-item">
            <span className="endpoint-label">{t("endpointHTTP")}</span>
            <code className="endpoint-url">{httpUrl}</code>
            <button className="btn-icon" title={t("copyHTTP")}
              onClick={() => onCopy(httpUrl, `${server.name}-http`)}>
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

  const [servers, setServers] = useState<ServerConfig[]>([]);
  const [listen, setListen] = useState("127.0.0.1:8765");
  const [ssePath, setSsePath] = useState("/api/v2/sse");
  const [httpPath, setHttpPath] = useState("/api/v2/mcp");
  const [adminToken, setAdminToken] = useState("");
  const [mcpToken, setMcpToken] = useState("");
  const [status, setStatus] = useState<GatewayProcessStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [configLoaded, setConfigLoaded] = useState(false);
  const [serversMode, setServersMode] = useState<"visual" | "json">("visual");
  const [jsonText, setJsonText] = useState("{}");
  const [jsonError, setJsonError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveSuccess, setSaveSuccess] = useState(false);
  const [configPath, setConfigPath] = useState<string>("");
  // 删除确认弹窗状态
  const [deleteConfirm, setDeleteConfirm] = useState<{ open: boolean; index: number; name: string }>({
    open: false, index: -1, name: ""
  });

  // ── 初始加载配置 ──
  useEffect(() => {
    loadLocalConfig().then((cfg) => {
      setServers(cfg.servers ?? []);
      if (cfg.listen) setListen(cfg.listen);
      if (cfg.transport?.sse?.basePath) setSsePath(cfg.transport.sse.basePath);
      if (cfg.transport?.streamableHttp?.basePath) setHttpPath(cfg.transport.streamableHttp.basePath);
      // 加载 Security 配置
      if (cfg.security?.admin?.token) setAdminToken(cfg.security.admin.token);
      if (cfg.security?.mcp?.token) setMcpToken(cfg.security.mcp.token);
      setJsonText(JSON.stringify(serversToJson(cfg.servers ?? []), null, 2));
      setConfigLoaded(true);
    }).catch((e) => setError(String(e)));
    // 获取配置文件路径
    getConfigPath().then(setConfigPath).catch(() => {});
  }, []);

  const switchToJson = () => {
    setJsonText(JSON.stringify(serversToJson(servers), null, 2));
    setJsonError(null);
    setServersMode("json");
  };
  const switchToVisual = () => {
    try {
      const parsed = JSON.parse(jsonText) as Record<string, unknown>;
      setServers(jsonToServers(parsed));
      setJsonError(null);
      setServersMode("visual");
    } catch {
      setJsonError(t("jsonParseError"));
    }
  };

  // ── 进程状态轮询 ──
  const refreshStatus = useCallback(async () => {
    try { setStatus(await getGatewayStatus()); } catch { /* ignore */ }
  }, []);
  useEffect(() => {
    void refreshStatus();
    const id = setInterval(() => { void refreshStatus(); }, 3000);
    return () => clearInterval(id);
  }, [refreshStatus]);

  const running = !!status?.running;

  const resolveServers = (): ServerConfig[] | null => {
    let list: ServerConfig[];
    if (serversMode === "json") {
      try {
        list = jsonToServers(JSON.parse(jsonText) as Record<string, unknown>);
      } catch {
        setJsonError(t("jsonParseErrorStart"));
        return null;
      }
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

  const persistConfig = async (nextServers: ServerConfig[]) => {
    const cfg: GatewayConfig = await loadLocalConfig();
    cfg.servers = nextServers;
    cfg.listen = listen;
    cfg.transport = { sse: { basePath: ssePath }, streamableHttp: { basePath: httpPath } };
    // 保存 Security 配置
    cfg.security = {
      admin: { enabled: adminToken !== "", token: adminToken },
      mcp: { enabled: mcpToken !== "", token: mcpToken },
    };
    await saveLocalConfig(cfg);
  };

  const handleStart = async () => {
    const nextServers = resolveServers();
    if (nextServers === null) return;
    setError(null); setBusy(true);
    try {
      await persistConfig(nextServers);
      await startGateway();
      await refreshStatus();
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  const handleStop = async () => {
    setError(null); setBusy(true);
    try { await stopGateway(); await refreshStatus(); }
    catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };

  // ── 独立保存配置 ──
  const handleSave = async () => {
    const nextServers = resolveServers();
    if (nextServers === null) return;
    setError(null); setSaving(true); setSaveSuccess(false);
    try {
      await persistConfig(nextServers);
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
    setDeleteConfirm({ open: false, index: -1, name: "" });
  };
  const cancelDelete = () => {
    setDeleteConfirm({ open: false, index: -1, name: "" });
  };

  const baseUrl = listen.startsWith("http") ? listen : `http://${listen}`;

  const handleCopy = async (url: string, key: string) => {
    await navigator.clipboard.writeText(url);
    setCopied(key);
    setTimeout(() => setCopied(null), 2000);
  };

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
          {/* 语言切换按钮 */}
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

      {/* ── 错误提示 ── */}
      {error && (
        <div className="alert alert-error" style={{ margin: "10px 20px 0" }}>
          {error}
          <button className="alert-close" onClick={() => setError(null)}>✕</button>
        </div>
      )}

      <div className="main-scroll">

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
              {/* 保存配置按钮 */}
              <button className="btn btn-secondary btn-sm" onClick={handleSave} disabled={saving || !configLoaded} title={t("saveConfig")}>
                <Save size={13} />
                {saving ? t("saving") : saveSuccess ? t("saveSuccess") : t("saveConfig")}
              </button>
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

          {serversMode === "visual" ? (
            <>
              {servers.length === 0 ? (
                <div className="empty-hint">{t("noServers")}</div>
              ) : (
                <div className="servers-list">
                  {servers.map((s, i) => (
                    <div className="server-block" key={i}>
                      {/* 每个 MCP 服务器独立的表头 */}
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
                        t={t}
                        onChange={(u) => setServers((prev) => prev.map((x, xi) => xi === i ? u : x))}
                        onDelete={() => requestDelete(i, s.name)}
                      />
                    </div>
                  ))}
                </div>
              )}
              <button className="btn btn-secondary btn-sm" style={{ marginTop: 10 }}
                onClick={() => setServers((prev) => [...prev, {
                  name: "", command: "npx", args: ["-y", ""],
                  description: "", cwd: "", env: {}, lifecycle: null, stdioProtocol: "auto", enabled: true,
                }])}>
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

      </div>

      {/* ── 底部通知条：配置文件位置 ── */}
      {configPath && (
        <div className="bottom-bar">
          <FolderOpen size={14} />
          <span className="bottom-bar-label">{t("configPath")}:</span>
          <code className="bottom-bar-path">{configPath}</code>
        </div>
      )}

      {/* ── 删除确认弹窗 ── */}
      <ConfirmDialog
        open={deleteConfirm.open}
        serverName={deleteConfirm.name}
        onCancel={cancelDelete}
        onConfirm={confirmDelete}
        t={t}
      />
    </div>
  );
}

export default App;

