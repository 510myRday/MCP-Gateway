import { ExternalLink, X } from "lucide-react";
import { open } from "@tauri-apps/plugin-shell";
import type { UpdateInfo } from "../hooks/useUpdateCheck";
import { useT, type Lang } from "../i18n";

interface UpdateBannerProps {
  info: UpdateInfo;
  lang: Lang;
  onDismiss: () => void;
}

export function UpdateBanner({ info, lang, onDismiss }: UpdateBannerProps) {
  const t = useT(lang);

  if (!info.hasUpdate) return null;

  const handleClick = async () => {
    try {
      await open(info.releaseUrl);
    } catch {
      // 打开失败静默忽略
    }
  };

  return (
    <div className="update-banner" role="status" aria-live="polite">
      <span className="update-banner-text">
        {t("updateAvailable")}&nbsp;<strong>{info.latestVersion}</strong>，{t("updateCurrentOutdated")}
      </span>
      <button
        className="update-banner-link"
        onClick={() => { void handleClick(); }}
        title={info.releaseUrl}
      >
        <ExternalLink size={12} />
        {t("updateViewRelease")}
      </button>
      <button
        className="update-banner-close"
        onClick={onDismiss}
        aria-label={t("updateDismiss")}
        title={t("updateDismiss")}
      >
        <X size={12} />
      </button>
    </div>
  );
}
