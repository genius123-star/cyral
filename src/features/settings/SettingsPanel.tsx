import { useCallback, useEffect, useState } from "react";

export interface AppSettings {
  autostart: boolean;
  city: string;
}

interface SettingsPanelProps {
  onLoad?: () => Promise<AppSettings>;
  onSave?: (settings: AppSettings) => Promise<unknown>;
}

export function SettingsPanel({ onLoad, onSave }: SettingsPanelProps) {
  const [settings, setSettings] = useState<AppSettings>({ autostart: false, city: "" });
  const [error, setError] = useState("");
  const [saved, setSaved] = useState(false);
  const load = useCallback(async () => {
    if (!onLoad) return;
    try {
      setSettings(await onLoad());
      setError("");
    } catch {
      setError("设置读取失败，请稍后重试。");
    }
  }, [onLoad]);

  useEffect(() => { void load(); }, [load]);

  const apply = async (next: AppSettings) => {
    setSettings(next);
    if (!onSave) return;
    setSaved(false);
    try {
      await onSave(next);
      setSaved(true);
      setError("");
    } catch {
      setError("设置保存失败，请稍后重试。");
    }
  };

  return (
    <section className="settings-panel app-preferences" aria-labelledby="app-preferences-heading">
      {error && <p className="todo-error">{error}</p>}
      <header className="app-preferences-heading">
        <span>APPLICATION</span>
        <h3 id="app-preferences-heading">应用偏好</h3>
      </header>
      <div className="app-preferences-row">
        <div className="preference-copy">
          <strong>开机自启</strong>
          <span>每次开机后自动启动 Cyral</span>
        </div>
        <label className="privacy-toggle">
          <input type="checkbox" aria-label="开机自启" checked={settings.autostart} onChange={(event) => void apply({ ...settings, autostart: event.target.checked })} />
          <span className="toggle-slider" />
        </label>
      </div>
      <div className="app-preferences-row city-preference-row">
        <label className="preference-copy" htmlFor="default-city">
          <strong>默认城市</strong>
          <span>用于开机问候天气，可选</span>
        </label>
        <input id="default-city" aria-label="默认城市" value={settings.city} onChange={(event) => setSettings({ ...settings, city: event.target.value })} onBlur={() => void apply(settings)} placeholder="例如：上海" />
      </div>
      {saved && <small className="preference-saved">设置已保存。</small>}
    </section>
  );
}