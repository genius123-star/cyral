import { useCallback, useEffect, useMemo, useState } from "react";

export type EmailProvider = "gmail" | "qq" | "netease" | "custom";
export type EmailTlsMode = "implicit" | "starttls";
export type InboxDetailLevel = "count" | "metadata";

export interface EmailAccountInput {
  id?: number;
  provider: EmailProvider;
  accountAddress: string;
  displayName: string;
  signature: string;
  smtpHost: string;
  smtpPort: number;
  smtpTls: EmailTlsMode;
  imapHost: string;
  imapPort: number;
  imapTls: EmailTlsMode;
  isDefault: boolean;
  inboxEnabled: boolean;
  inboxIntervalMinutes: 5 | 10 | 30;
  inboxDetailLevel: InboxDetailLevel;
  enabled: boolean;
}

export interface EmailAccountView extends EmailAccountInput {
  id: number;
  credentialConfigured: boolean;
}

interface EmailSettingsProps {
  onLoad?: () => Promise<EmailAccountView[]>;
  onSave?: (account: EmailAccountInput, credential?: string) => Promise<EmailAccountView>;
  onDelete?: (id: number) => Promise<unknown>;
  onConnectGmail?: (id: number) => Promise<EmailAccountView>;
  onDisconnectGmail?: (id: number) => Promise<EmailAccountView>;
  onTestConnection?: (id: number) => Promise<string>;
}

const emptyAccount = (): EmailAccountInput => ({
  provider: "gmail",
  accountAddress: "",
  displayName: "",
  signature: "",
  smtpHost: "",
  smtpPort: 465,
  smtpTls: "implicit",
  imapHost: "",
  imapPort: 993,
  imapTls: "implicit",
  isDefault: true,
  inboxEnabled: false,
  inboxIntervalMinutes: 10,
  inboxDetailLevel: "count",
  enabled: true,
});

function providerName(provider: EmailProvider) {
  return { gmail: "Gmail", qq: "QQ 邮箱", netease: "网易邮箱", custom: "自定义邮箱" }[provider];
}

export function EmailSettings({ onLoad, onSave, onDelete, onConnectGmail, onDisconnectGmail, onTestConnection }: EmailSettingsProps) {
  const [accounts, setAccounts] = useState<EmailAccountView[]>([]);
  const [draft, setDraft] = useState<EmailAccountInput>(emptyAccount);
  const [credential, setCredential] = useState("");
  const [error, setError] = useState("");
  const [notice, setNotice] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    if (!onLoad) return;
    try {
      const loaded = await onLoad();
      setAccounts(loaded);
      setDraft((current) => {
        if (current.id !== undefined || current.accountAddress.trim() !== "") return current;
        return loaded[0] ?? current;
      });
      setError("");
    } catch {
      setError("邮箱账户读取失败，请稍后重试。");
    }
  }, [onLoad]);

  useEffect(() => { void load(); }, [load]);

  const editing = useMemo(
    () => accounts.find((account) => account.id === draft.id),
    [accounts, draft.id],
  );

  const save = async () => {
    if (!onSave || busy) return;
    setBusy(true);
    setNotice("");
    try {
      const result = await onSave(draft, credential || undefined);
      setAccounts((current) => [result, ...current.filter((item) => item.id !== result.id)]);
      setDraft(result);
      setCredential("");
      setNotice("邮箱设置已保存。");
      setError("");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-panel app-preferences email-settings" aria-labelledby="email-settings-heading">
      <header className="app-preferences-heading">
        <span>MAIL</span>
        <h3 id="email-settings-heading">邮件助手</h3>
        <p>后台准备邮件并检查新邮件；发送必须由你在审核卡确认。</p>
      </header>

      {accounts.length > 0 && (
        <div className="email-account-list" aria-label="已配置邮箱">
          {accounts.map((account) => (
            <button
              type="button"
              key={account.id}
              className={draft.id === account.id ? "email-account active" : "email-account"}
              onClick={() => { setDraft(account); setCredential(""); setNotice(""); }}
            >
              <strong>{providerName(account.provider)}</strong>
              <span>{account.accountAddress}</span>
              <small>{account.credentialConfigured ? "凭据已保存" : "尚未连接"}</small>
            </button>
          ))}
          <button type="button" className="email-account add" onClick={() => { setDraft(emptyAccount()); setCredential(""); }}>＋ 添加账户</button>
        </div>
      )}

      <div className="email-form-grid">
        <label>服务商
          <select value={draft.provider} onChange={(event) => setDraft({ ...draft, provider: event.target.value as EmailProvider })}>
            <option value="gmail">Gmail</option><option value="qq">QQ 邮箱</option><option value="netease">网易邮箱</option><option value="custom">自定义邮箱</option>
          </select>
        </label>
        <label>邮箱地址<input type="email" value={draft.accountAddress} onChange={(event) => setDraft({ ...draft, accountAddress: event.target.value })} /></label>
        <label>发件人名称<input value={draft.displayName} onChange={(event) => setDraft({ ...draft, displayName: event.target.value })} /></label>
        {draft.provider !== "gmail" && (
          <label>授权码或应用密码<input type="password" value={credential} placeholder={editing?.credentialConfigured ? "已保存；留空表示不修改" : "只写入 Windows 凭据管理器"} onChange={(event) => setCredential(event.target.value)} /></label>
        )}
        {draft.provider === "custom" && <>
          <label>SMTP 服务器<input value={draft.smtpHost} onChange={(event) => setDraft({ ...draft, smtpHost: event.target.value })} /></label>
          <label>SMTP 端口<input type="number" value={draft.smtpPort} onChange={(event) => setDraft({ ...draft, smtpPort: Number(event.target.value) })} /></label>
          <label>IMAP 服务器<input value={draft.imapHost} onChange={(event) => setDraft({ ...draft, imapHost: event.target.value })} /></label>
          <label>IMAP 端口<input type="number" value={draft.imapPort} onChange={(event) => setDraft({ ...draft, imapPort: Number(event.target.value) })} /></label>
        </>}
        <label className="email-signature">默认签名<textarea value={draft.signature} onChange={(event) => setDraft({ ...draft, signature: event.target.value })} /></label>
      </div>

      <div className="app-preferences-row">
        <div className="preference-copy"><strong>新邮件提醒</strong><span>首次开启只建立基线，不提醒历史邮件</span></div>
        <label className="privacy-toggle"><input type="checkbox" checked={draft.inboxEnabled} onChange={(event) => setDraft({ ...draft, inboxEnabled: event.target.checked })} /><span className="toggle-slider" /></label>
      </div>
      {draft.inboxEnabled && <div className="email-form-grid compact">
        <label>检查周期<select value={draft.inboxIntervalMinutes} onChange={(event) => setDraft({ ...draft, inboxIntervalMinutes: Number(event.target.value) as 5 | 10 | 30 })}><option value={5}>5 分钟</option><option value={10}>10 分钟</option><option value={30}>30 分钟</option></select></label>
        <label>提醒内容<select value={draft.inboxDetailLevel} onChange={(event) => setDraft({ ...draft, inboxDetailLevel: event.target.value as InboxDetailLevel })}><option value="count">仅显示数量</option><option value="metadata">发件人与主题</option></select></label>
      </div>}

      {draft.provider === "gmail" && <p className="email-note">Gmail 使用 OAuth 授权，不接受手动输入密码。</p>}
      {error && <p className="todo-error">{error}</p>}
      {notice && <small className="preference-saved">{notice}</small>}
      <div className="email-actions">
        <button type="button" onClick={() => void save()} disabled={busy}>{busy ? "处理中…" : "保存账户"}</button>
        {draft.provider === "gmail" && draft.id && !editing?.credentialConfigured && onConnectGmail && <button type="button" className="secondary" disabled={busy} onClick={async () => { setBusy(true); setNotice("正在等待 Google 授权并完成本地凭据校验…"); setError(""); try { const connected = await onConnectGmail(draft.id!); setAccounts((current) => [connected, ...current.filter((item) => item.id !== connected.id)]); setDraft(connected); setNotice("Gmail 已连接，令牌已安全保存并通过回读校验。"); } catch (reason) { setNotice(""); setError(reason instanceof Error ? reason.message : String(reason)); } finally { setBusy(false); } }}>连接 Gmail</button>}
        {draft.provider === "gmail" && draft.id && editing?.credentialConfigured && onDisconnectGmail && <button type="button" className="secondary" disabled={busy} onClick={async () => { setBusy(true); try { const disconnected = await onDisconnectGmail(draft.id!); setAccounts((current) => [disconnected, ...current.filter((item) => item.id !== disconnected.id)]); setDraft(disconnected); } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); } finally { setBusy(false); } }}>断开 Gmail</button>}
        {draft.id && editing?.credentialConfigured && onTestConnection && <button type="button" className="secondary" disabled={busy} onClick={async () => { setBusy(true); setNotice(""); try { setNotice(await onTestConnection(draft.id!)); setError(""); } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); } finally { setBusy(false); } }}>测试连接</button>}
        {draft.id && onDelete && <button type="button" className="secondary" onClick={async () => { await onDelete(draft.id!); setDraft(emptyAccount()); await load(); }}>停用并删除</button>}
      </div>
    </section>
  );
}
