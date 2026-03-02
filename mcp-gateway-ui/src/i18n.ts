// ── 中英文翻译字典 ─────────────────────────────────────────────────
export type Lang = "zh" | "en";

const translations = {
  zh: {
    // 顶栏
    appTitle: "MCP 网关",
    running: "运行中",
    stopped: "已停止",
    starting: "启动中…",
    stopping: "停止中…",
    start: "启动",
    stop: "停止",

    // 网关设置
    gatewaySettings: "网关设置",
    listenAddress: "监听地址",
    ssePath: "SSE 路径",
    httpStreamPath: "HTTP 流路径",

    // Security
    securityConfig: "安全配置",
    adminToken: "Admin Token",
    mcpToken: "MCP Token",
    tokenPlaceholder: "留空则禁用",

    // MCP Servers
    mcpServers: "MCP 服务列表",
    name: "名称",
    command: "命令",
    args: "参数",
    cwd: "工作目录",
    env: "环境变量",
    envHint: "每行一个，格式：KEY=VALUE",
    envPlaceholder: "每行一个环境变量，格式：KEY=VALUE",
    envVars: "环境变量",
    addEnvVar: "添加环境变量",
    removeEnvVar: "删除环境变量",
    addServer: "＋ 添加服务",
    noServers: "暂无服务 — 点击「添加服务」或切换到 JSON 模式粘贴配置。",
    enabledClick: "已启用 — 点击禁用",
    disabledClick: "已禁用 — 点击启用",
    remove: "删除",

    // 端点链接
    copySSE: "复制 SSE 链接",
    copyHTTP: "复制 HTTP 链接",
    noEnabledServers: "无启用服务 — 请添加服务后重启",
    endpointSSE: "SSE",
    endpointHTTP: "HTTP",

    // JSON 编辑器
    jsonHint: "粘贴 mcpServers 格式 — 与 claude_desktop_config.json 相同",
    jsonParseError: "JSON 解析失败，请检查格式",
    jsonParseErrorStart: "JSON 解析失败，无法启动",
    allServersInvalid: "所有服务都缺少名称或命令，请填写后再启动",

    // 错误
    errorTitle: "错误",
    portOccupied: "端口被占用",
    portKillSuccess: "已杀死占用进程，正在重试启动…",
    portKillFail: "无法清理端口占用进程",

    // 模式
    visual: "可视化",
    json: "JSON",

    // 保存配置
    saveConfig: "保存配置",
    saving: "保存中…",
    saveSuccess: "配置已保存",

    // 删除确认
    confirmDeleteTitle: "确认删除",
    confirmDeleteMsg: '确定要删除服务 "{name}" 吗？此操作无法撤销。',
    cancel: "取消",
    confirmDelete: "删除",

    // JSON格式化
    formatJson: "格式化 JSON",
    formatError: "JSON 格式化失败，请检查语法",

    // 底部通知条
    configPath: "配置文件位置",

    // 语言切换
    langToggle: "English",
  },
  en: {
    // Topbar
    appTitle: "MCP Gateway",
    running: "Running",
    stopped: "Stopped",
    starting: "Starting…",
    stopping: "Stopping…",
    start: "Start",
    stop: "Stop",

    // Gateway settings
    gatewaySettings: "Gateway Settings",
    listenAddress: "Listen Address",
    ssePath: "SSE Path",
    httpStreamPath: "HTTP Stream Path",

    // Security
    securityConfig: "Security",
    adminToken: "Admin Token",
    mcpToken: "MCP Token",
    tokenPlaceholder: "Leave empty to disable",

    // MCP Servers
    mcpServers: "MCP Servers",
    name: "Name",
    command: "Command",
    args: "Args",
    cwd: "Working Directory",
    env: "Environment Variables",
    envHint: "One per line, format: KEY=VALUE",
    envPlaceholder: "One environment variable per line, format: KEY=VALUE",
    envVars: "Environment Variables",
    addEnvVar: "Add Environment Variable",
    removeEnvVar: "Remove Environment Variable",
    addServer: "＋ Add Server",
    noServers: "No servers yet — click Add Server or switch to JSON to paste config.",
    enabledClick: "Enabled — click to disable",
    disabledClick: "Disabled — click to enable",
    remove: "Remove",

    // Endpoint links
    copySSE: "Copy SSE URL",
    copyHTTP: "Copy HTTP URL",
    noEnabledServers: "No enabled servers — add a server and restart",
    endpointSSE: "SSE",
    endpointHTTP: "HTTP",

    // JSON editor
    jsonHint: "Paste mcpServers format — same as claude_desktop_config.json",
    jsonParseError: "JSON parse error — please check the format",
    jsonParseErrorStart: "JSON parse error — cannot start",
    allServersInvalid: "All servers are missing name or command, please fill them in",

    // Errors
    errorTitle: "Error",
    portOccupied: "Port is occupied",
    portKillSuccess: "Killed occupying process, retrying start…",
    portKillFail: "Failed to clear port occupying process",

    // Mode
    visual: "Visual",
    json: "JSON",

    // Save config
    saveConfig: "Save",
    saving: "Saving…",
    saveSuccess: "Config saved",

    // Delete confirmation
    confirmDeleteTitle: "Confirm Delete",
    confirmDeleteMsg: 'Are you sure you want to delete server "{name}"? This cannot be undone.',
    cancel: "Cancel",
    confirmDelete: "Delete",

    // JSON format
    formatJson: "Format JSON",
    formatError: "JSON format failed, please check syntax",

    // Bottom notification bar
    configPath: "Config file location",

    // Language toggle
    langToggle: "中文",
  },
} as const;

export type TKey = keyof typeof translations.zh;

export function useT(lang: Lang) {
  return (key: TKey): string => translations[lang][key];
}

