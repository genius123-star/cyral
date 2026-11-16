import { useEffect, useState, type FormEvent } from "react";

export interface VisionProviderInput {
  baseUrl: string;
  protocol: "responses" | "chat_completions";
  model: string;
  apiKey: string;
  enabled: boolean;
}

export type SavedVisionProvider = Omit<VisionProviderInput, "apiKey">;

interface VisionSettingsProps {
  onSave: (settings: VisionProviderInput) => Promise<unknown>;
  onLoad?: () => Promise<SavedVisionProvider | null>;
}

export function VisionSettings({ onSave, onLoad }: VisionSettingsProps) {
  const [settings, setSettings] = useState<VisionProviderInput>({
    baseUrl: "",
    protocol: "chat_completions",
    model: "",
    apiKey: "",
    enabled: false,
  });
  const [saveState, setSaveState] = useState<
    { kind: "idle" | "saving" | "success" | "error"; message?: string }
  >({ kind: "idle" });

  useEffect(() => {
    if (!onLoad) return;
    let cancelled = false;
    void onLoad().then((saved) => {
      if (!cancelled && saved) {
        setSettings({ ...saved, apiKey: "" });
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
      setSaveState({ kind: "success", message: "视觉配置已保存" });
    } catch (error) {
      setSaveState({
        kind: "error",
        message: error instanceof Error ? error.message : String(error),
      });
    }
  };

  return (
    <form className="model-settings" onSubmit={submit}>
      <div className="vision-toggle">
        <label>
          <input type="checkbox" checked={settings.enabled} onChange={(event) => {
            setSettings({ ...settings, enabled: event.target.checked });
          }} />
          启用视觉分析（截图后由视觉模型理解场景）
        </label>
        {settings.enabled && <div className="local-model-note">未配置或视觉模型调用失败时，自动降级为纯文本摘要，仅使用窗口标题与进程名。</div>}
      </div>
      {settings.enabled && (
        <>
          <label>Base URL<input value={settings.baseUrl} placeholder="https://api.openai.com/v1" onChange={(event) => setSettings({ ...settings, baseUrl: event.target.value })} /></label>
          <label>模型 ID<input value={settings.model} placeholder="gpt-5.6-luna" onChange={(event) => setSettings({ ...settings, model: event.target.value })} /></label>
          <label>
            API Key
            <span className="api-key-field">
              <input type="password" autoComplete="off" value={settings.apiKey} onChange={(event) => setSettings({ ...settings, apiKey: event.target.value })} />
            </span>
          </label>
          <details className="model-advanced-settings">
            <summary>高级设置</summary>
            <label>接口协议<select value={settings.protocol} onChange={(event) => setSettings({ ...settings, protocol: event.target.value as VisionProviderInput["protocol"] })}><option value="chat_completions">Chat Completions</option><option value="responses">Responses API</option></select></label>
          </details>
        </>
      )}
      <button type="submit" disabled={saveState.kind === "saving"}>
        {saveState.kind === "saving" ? "正在保存…" : "保存视觉配置"}
      </button>
      {saveState.kind === "success" && <div className="model-save-toast" role="status">{saveState.message}</div>}
      {saveState.kind === "error" && <div className="model-save-toast error" role="alert">保存失败：{saveState.message}</div>}
    </form>
  );
}