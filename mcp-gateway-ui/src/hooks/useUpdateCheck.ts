import { useState, useEffect } from "react";

export interface UpdateInfo {
  hasUpdate: boolean;
  latestVersion: string;
  releaseUrl: string;
  releaseNotes: string;
}

const GITHUB_OWNER = "510myRday";
const GITHUB_REPO = "MCP-Gateway";
const CHECK_INTERVAL_MS = 24 * 60 * 60 * 1000; // 24 小时
const CACHE_KEY = "mcp-update-check";

interface CachedResult {
  checkedAt: number;
  latestVersion: string;
  releaseUrl: string;
  releaseNotes: string;
}

function parseSemver(v: string | null | undefined): number[] {
  return (v ?? "")
    .replace(/^v/, "")
    .split(".")
    .map((n) => parseInt(n, 10) || 0);
}

function isNewerVersion(latest: string, current: string): boolean {
  const l = parseSemver(latest);
  const c = parseSemver(current);
  for (let i = 0; i < Math.max(l.length, c.length); i++) {
    const lv = l[i] ?? 0;
    const cv = c[i] ?? 0;
    if (lv > cv) return true;
    if (lv < cv) return false;
  }
  return false;
}

async function fetchLatestRelease(currentVersion: string): Promise<UpdateInfo> {
  const url = `https://api.github.com/repos/${GITHUB_OWNER}/${GITHUB_REPO}/releases/latest`;
  const controller = new AbortController();
  const timeoutId = setTimeout(() => controller.abort(), 8000);
  try {
    const res = await fetch(url, {
      headers: { Accept: "application/vnd.github+json" },
      signal: controller.signal,
    });
    if (!res.ok) {
      throw new Error(`GitHub API returned ${res.status}`);
    }
    const data = (await res.json()) as {
      tag_name: string;
      html_url: string;
      body?: string;
    };
    const latestVersion = data.tag_name ?? "";
    return {
      hasUpdate: isNewerVersion(latestVersion, currentVersion),
      latestVersion,
      releaseUrl: data.html_url ?? "",
      releaseNotes: data.body ?? "",
    };
  } finally {
    clearTimeout(timeoutId);
  }
}

export function useUpdateCheck(currentVersion: string): UpdateInfo | null {
  const [updateInfo, setUpdateInfo] = useState<UpdateInfo | null>(null);

  useEffect(() => {
    let cancelled = false;

    const run = async () => {
      // 读取缓存，24h 内不重复请求
      try {
        const raw = localStorage.getItem(CACHE_KEY);
        if (raw) {
          const cached = JSON.parse(raw) as Partial<CachedResult> & {
            result?: UpdateInfo;
          };
          const checkedAt =
            typeof cached.checkedAt === "number" ? cached.checkedAt : 0;
          if (Date.now() - checkedAt < CHECK_INTERVAL_MS) {
            const latestVersion =
              typeof cached.latestVersion === "string"
                ? cached.latestVersion
                : typeof cached.result?.latestVersion === "string"
                  ? cached.result.latestVersion
                  : "";
            const releaseUrl =
              typeof cached.releaseUrl === "string"
                ? cached.releaseUrl
                : cached.result?.releaseUrl ?? "";
            const releaseNotes =
              typeof cached.releaseNotes === "string"
                ? cached.releaseNotes
                : cached.result?.releaseNotes ?? "";
            const result: UpdateInfo = {
              hasUpdate: isNewerVersion(latestVersion, currentVersion),
              latestVersion,
              releaseUrl,
              releaseNotes,
            };
            if (!cancelled) setUpdateInfo(result);
            return;
          }
        }
      } catch {
        // 缓存读取失败，继续发起请求
      }

      try {
        const result = await fetchLatestRelease(currentVersion);
        if (cancelled) return;
        setUpdateInfo(result);
        // 写入缓存
        const cache: CachedResult = {
          checkedAt: Date.now(),
          latestVersion: result.latestVersion,
          releaseUrl: result.releaseUrl,
          releaseNotes: result.releaseNotes,
        };
        localStorage.setItem(CACHE_KEY, JSON.stringify(cache));
      } catch {
        // 网络失败静默忽略，不影响主功能
      }
    };

    void run();
    return () => {
      cancelled = true;
    };
  }, [currentVersion]);

  return updateInfo;
}
