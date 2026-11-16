import { useEffect, useRef, useState, type FormEvent } from "react";

export interface ModelProviderInput {
  providerName: string;
  baseUrl: string;
  protocol: "auto" | "responses" | "chat_completions";
  model: string;
  apiKey: string;
  monthlyBudgetUsd: number;
}

export type SavedModelProvider = Omit<ModelProviderInput, "apiKey">;

const DEFAULT_CLOUD_SETTINGS: ModelProviderInput = {
  providerName: "",
  baseUrl: "https://api.openai.com/v1",
  protocol: "auto",
  model: "gpt-5.6-luna",
  apiKey: "",
  monthlyBudgetUsd: 10,
};

interface ModelSettingsProps {
  onSave: (settings: ModelProviderInput) => unknown | Promise<unknown>;
  onLoad?: () => Promise<SavedModelProvider | null>;
}

export function ModelSettings({ onSave, onLoad }: ModelSettingsProps) {
  const [localMode, setLocalMode] = useState(false);
  const [hasSavedCloudConfig, setHasSavedCloudConfig] = useState(false);
  const cloudSettings = useRef<ModelProviderInput>(DEFAULT_CLOUD_SETTINGS);
  const [keyVisible, setKeyVisible] = useState(false);
  const [saveState, setSaveState] = useState<
    { kind: "idle" | "saving" | "success" | "error"; message?: string }
  >({ kind: "idle" });
  const [settings, setSettings] = useState<ModelProviderInput>(DEFAULT_CLOUD_SETTINGS);

  useEffect(() => {
    if (!onLoad) return;
    let cancelled = false;
    void onLoad().then((saved) => {
      if (!cancelled && saved) {
        const loaded = { ...saved, apiKey: "" };
        const isLocal = /^https?:\/\/(localhost|127\.0\.0\.1|\[::1\])(?::|\/)/i.test(saved.baseUrl);
        setSettings(loaded);
        setLocalMode(isLocal);
        if (!isLocal) {
          cloudSettings.current = loaded;
          setHasSavedCloudConfig(true);
        }
      }
    });
    return () => {
      cancelled = true;
    };
  }, [onLoad]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setSaveState({ kind: "saving" });
    try {
      await onSave({ ...settings, baseUrl: settings.baseUrl.replace(/\/$/, "") });
      if (!localMode) setHasSavedCloudConfig(true);
      setSaveState({ kind: "success", message: "配置已安全保存" });
    } catch (error) {
      setSaveState({
        kind: "error",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  };

  return (
    <form className="model-settings" onSubmit={submit}>
      <label className="local-model-toggle">
        <input type="checkbox" checked={localMode} onChange={(event) => {
          const enabled = event.target.checked;
          setLocalMode(enabled);
          if (enabled) {
            cloudSettings.current = settings;
            setSettings({ ...settings, providerName: "本地模型", baseUrl: "http://localhost:1234/v1", model: "", apiKey: "" });
          } else {
            setSettings(cloudSettings.current);
          }
        }} />
        使用本地模型
      </label>
      {localMode && <div className="local-model-note">仅连接本机回环地址；LM Studio 默认可用 <code>localhost:1234/v1</code>，Ollama 通常使用 <code>localhost:11434/v1</code>。API Key 可留空。</div>}
      <label>供应商名称（可选）<input value={settings.providerName} placeholder="自定义供应商" onChange={(event) => setSettings({ ...settings, providerName: event.target.value })} /></label>
      <label>Base URL<input value={settings.baseUrl} onChange={(event) => setSettings({ ...settings, baseUrl: event.target.value })} /></label>
      <details className="model-advanced-settings">
        <summary>高级设置</summary>
        <label>接口协议<select value={settings.protocol} onChange={(event) => setSettings({ ...settings, protocol: event.target.value as ModelProviderInput["protocol"] })}><option value="auto">自动检测</option><option value="responses">Responses API</option><option value="chat_completions">Chat Completions</option></select></label>
      </details>
      <label>模型 ID<input value={settings.model} onChange={(event) => setSettings({ ...settings, model: event.target.value })} /></label>
      <label>
        API Key{localMode ? "（本地可选）" : ""}
        <span className="api-key-field">
          <input
            type={keyVisible ? "text" : "password"}
            autoComplete="off"
            value={settings.apiKey}
            onChange={(event) => setSettings({ ...settings, apiKey: event.target.value })}
          />
          <button
            className="api-key-visibility"
            type="button"
            aria-label={keyVisible ? "隐藏 API Key" : "显示 API Key"}
            aria-pressed={keyVisible}
            onClick={() => setKeyVisible((visible) => !visible)}
          >
            {keyVisible ? (
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 3l18 18M10.6 10.7a2 2 0 002.7 2.7M9.9 4.2A10.7 10.7 0 0112 4c5.5 0 9 6 9 6a17 17 0 01-2.1 2.8M6.2 6.2C3.9 7.7 3 10 3 10s3.5 6 9 6a9.8 9.8 0 004-0.8" /></svg>
            ) : (
              <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 12s3.5-6 9-6 9 6 9 6-3.5 6-9 6-9-6-9-6z" /><circle cx="12" cy="12" r="2.5" /></svg>
            )}
          </button>
        </span>
      </label>
      {!localMode && hasSavedCloudConfig && (
        <div className="local-model-note" role="status">
          已保存的 API Key 位于 Windows 凭据管理器，不会在这里回显；留空保存会继续使用原密钥。
        </div>
      )}
      <label>月度预算（美元）<input type="number" min="1" max="10" step="1" value={settings.monthlyBudgetUsd} onChange={(event) => setSettings({ ...settings, monthlyBudgetUsd: Number(event.target.value) })} /></label>
      <button type="submit" disabled={saveState.kind === "saving"}>
        {saveState.kind === "saving" ? "正在保存…" : "保存模型配置"}
      </button>
      {saveState.kind === "success" && <div className="model-save-toast" role="status">{saveState.message}</div>}
      {saveState.kind === "error" && <div className="model-save-toast error" role="alert">保存失败：{saveState.message}</div>}
    </form>
  );
}
