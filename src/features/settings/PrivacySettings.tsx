import { type ReactNode, useEffect, useRef, useState } from "react";

export interface PrivacyConfig {
  screenObservation: boolean;
  autoLocation: boolean;
  proactiveInteraction: boolean;
  fileSearchDesktop: boolean;
  fileSearchDocuments: boolean;
  fileSearchDownloads: boolean;
}

interface Props {
  onLoad?: () => Promise<PrivacyConfig>;
  onSave?: (config: PrivacyConfig) => Promise<PrivacyConfig>;
  children?: ReactNode;
}

interface PermissionRowProps {
  title: string;
  description: string;
  label: string;
  checked: boolean;
  disabled: boolean;
  onToggle: () => void;
  className?: string;
}

function PermissionRow({ title, description, label, checked, disabled, onToggle, className = "privacy-permission-row" }: PermissionRowProps) {
  return (
    <div className={className}>
      <div className="permission-copy">
        <strong>{title}</strong>
        <span>{description}</span>
      </div>
      <label className="privacy-toggle">
        <input aria-label={label} type="checkbox" checked={checked} onChange={onToggle} disabled={disabled} />
        <span className="toggle-slider" />
      </label>
    </div>
  );
}

function samePrivacyConfig(left: PrivacyConfig, right: PrivacyConfig) {
  return left.screenObservation === right.screenObservation
    && left.autoLocation === right.autoLocation
    && left.proactiveInteraction === right.proactiveInteraction
    && left.fileSearchDesktop === right.fileSearchDesktop
    && left.fileSearchDocuments === right.fileSearchDocuments
    && left.fileSearchDownloads === right.fileSearchDownloads;
}

export function PrivacySettings({ onLoad, onSave, children }: Props) {
  const [config, setConfig] = useState<PrivacyConfig>({
    screenObservation: false,
    autoLocation: false,
    proactiveInteraction: true,
    fileSearchDesktop: false,
    fileSearchDocuments: false,
    fileSearchDownloads: false,
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [status, setStatus] = useState<{ kind: "saved" | "error"; message: string }>();
  const saveInFlight = useRef(false);

  useEffect(() => {
    if (!onLoad) {
      setLoading(false);
      return;
    }
    let cancelled = false;
    void onLoad()
      .then((loaded) => {
        if (!cancelled) setConfig(loaded);
      })
      .catch(() => {
        if (!cancelled) {
          setStatus({ kind: "error", message: "隐私设置读取失败，请重新打开此页面" });
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [onLoad]);

  const handleToggle = async (
    key: keyof PrivacyConfig,
    subject: string,
    enabledText = "已开启",
    disabledText = "已关闭",
  ) => {
    if (saveInFlight.current) return;
    const previous = config;
    const next = { ...previous, [key]: !previous[key] };
    setConfig(next);
    setStatus(undefined);
    if (!onSave) return;

    saveInFlight.current = true;
    setSaving(true);
    try {
      const persisted = await onSave(next);
      if (!samePrivacyConfig(persisted, next)) {
        throw new Error("persisted privacy settings do not match");
      }
      setConfig(persisted);
      setStatus({
        kind: "saved",
        message: `${subject}${persisted[key] ? enabledText : disabledText}`,
      });
    } catch {
      setConfig(previous);
      setStatus({
        kind: "error",
        message: `${key.startsWith("fileSearch") ? `${subject}授权` : subject}保存失败，已恢复原状态`,
      });
    } finally {
      saveInFlight.current = false;
      setSaving(false);
    }
  };

  if (loading) return <div className="privacy-settings">加载中...</div>;

  return (
    <div className="privacy-settings">
      <section className="privacy-access-card" aria-labelledby="privacy-basic-heading">
        <header>
          <span className="privacy-eyebrow">PRIVACY CONTROLS</span>
          <h3 id="privacy-basic-heading">基础授权</h3>
        </header>
        <PermissionRow title="屏幕观察" label="屏幕观察" checked={config.screenObservation} disabled={saving} onToggle={() => void handleToggle("screenObservation", "屏幕观察授权")} description={config.screenObservation ? "用于陪伴模式下的场景理解。" : "开启后可在对话中输入 /陪伴模式 启用。"} />
        <PermissionRow title="主动关心" label="主动关心" checked={config.proactiveInteraction} disabled={saving} onToggle={() => void handleToggle("proactiveInteraction", "主动关心")} description="关闭后仍可聊天和接收待办提醒。" />
        <PermissionRow title="地理位置" label="地理位置" checked={config.autoLocation} disabled={saving} onToggle={() => void handleToggle("autoLocation", "地理位置授权")} description="用于自动回答天气问题。" />
      </section>

      <section className="privacy-file-search" aria-labelledby="file-search-heading">
        <header>
          <span className="privacy-eyebrow">LOCAL FILES</span>
          <h3 id="file-search-heading">本地文件搜索</h3>
          <p>只按文件名和元数据查找，不读取文件内容。授权变更会立即保存。</p>
        </header>
        <PermissionRow className="file-search-row desktop" title="桌面" label="搜索桌面文件" checked={config.fileSearchDesktop} disabled={saving} onToggle={() => void handleToggle("fileSearchDesktop", "桌面目录", "已授权", "授权已关闭")} description="搜索桌面中的文件名" />
        <PermissionRow className="file-search-row documents" title="文档" label="搜索文档文件" checked={config.fileSearchDocuments} disabled={saving} onToggle={() => void handleToggle("fileSearchDocuments", "文档目录", "已授权", "授权已关闭")} description="搜索文档中的文件名" />
        <PermissionRow className="file-search-row downloads" title="下载" label="搜索下载文件" checked={config.fileSearchDownloads} disabled={saving} onToggle={() => void handleToggle("fileSearchDownloads", "下载目录", "已授权", "授权已关闭")} description="搜索下载中的文件名" />
      </section>

      <div className="privacy-notice">
        <p>⚠ 文件内容不会离开本机；文件名和相对路径可能发送给已配置的模型供应商，用于整理搜索结果。</p>
      </div>
      {status && <p className={`privacy-save-status ${status.kind === "saved" ? "success" : "error"}`} role="status">{status.message}</p>}
      {children}
    </div>
  );
}