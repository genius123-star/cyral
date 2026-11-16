import { useCallback, useEffect, useState } from "react";

export interface EmailDraftReview {
  id: string;
  accountId: number;
  accountLabel: string;
  recipients: string[];
  subject: string;
  body: string;
  attachmentLabels: string[];
  revision: number;
  status: "preparing" | "waiting_for_user" | "sending" | "sent" | "failed" | "unknown" | "cancelled";
  statusMessage?: string;
}

interface EmailReviewCardProps {
  reviewId?: string;
  result?: EmailDraftReview;
  onLoad?: (id: string) => Promise<EmailDraftReview>;
  onUpdate?: (review: EmailDraftReview) => Promise<EmailDraftReview>;
  onCancel?: (id: string) => Promise<unknown>;
  onSend?: (id: string, revision: number) => Promise<EmailDraftReview>;
}

export function EmailReviewCard({ reviewId, result, onLoad, onUpdate, onCancel, onSend }: EmailReviewCardProps) {
  const [review, setReview] = useState<EmailDraftReview>();
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    if (!reviewId || !onLoad) return;
    try {
      const loaded = await onLoad(reviewId);
      setReview(result?.id === reviewId ? result : loaded);
      setError("");
    }
    catch { setError("邮件审核内容已失效，请重新通过对话准备。"); }
  }, [reviewId, result, onLoad]);
  useEffect(() => { void load(); }, [load]);
  useEffect(() => {
    if (result?.id === reviewId) {
      setReview(result);
      setError("");
      setBusy(false);
    }
  }, [result, reviewId]);
  if (!reviewId) return null;
  if (!review) return <section className="email-review-card"><p>{error || "正在读取邮件草稿…"}</p></section>;

  const saveEdits = async () => {
    if (!onUpdate || busy) return;
    setBusy(true);
    try { setReview(await onUpdate(review)); setError(""); }
    catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); }
    finally { setBusy(false); }
  };

  const editable = review.status === "waiting_for_user";
  const statusText = {
    preparing: "正在准备邮件…",
    waiting_for_user: "请检查内容，确认无误后由你亲自发送。",
    sending: "正在等待邮箱服务确认发送结果…",
    sent: review.statusMessage || "邮件已发送，邮箱服务已经确认接收。",
    failed: review.statusMessage || "邮件发送失败，没有自动重试。",
    unknown: review.statusMessage || "发送结果无法确认；为避免重复发送，Cyral 不会自动重试。",
    cancelled: "这封邮件已取消，没有发送。",
  }[review.status];

  return <section className="email-review-card" aria-labelledby="email-review-heading">
    <span>REVIEW BEFORE SENDING</span><h3 id="email-review-heading">发送前检查</h3>
    <p className="email-account-label">发件账户：{review.accountLabel}</p>
    <label>收件人<input disabled={!editable} value={review.recipients.join(", ")} onChange={(event) => setReview({ ...review, recipients: event.target.value.split(",").map((item) => item.trim()).filter(Boolean) })} /></label>
    <label>主题<input disabled={!editable} value={review.subject} onChange={(event) => setReview({ ...review, subject: event.target.value })} /></label>
    <label>正文<textarea disabled={!editable} value={review.body} onChange={(event) => setReview({ ...review, body: event.target.value })} /></label>
    {review.attachmentLabels.length > 0 && <p>附件：{review.attachmentLabels.join("、")}</p>}
    <p className={`email-send-status ${review.status}`} role={review.status === "failed" || review.status === "unknown" ? "alert" : "status"} aria-live="polite">{statusText}</p>
    {error && <p className="todo-error">{error}</p>}
    <div className="email-actions">
      <button type="button" className="secondary" disabled={busy || !editable} onClick={() => void saveEdits()}>保存修改并重新确认</button>
      <button type="button" disabled={busy || !onSend || !editable} onClick={async () => { if (!onSend) return; setBusy(true); setError(""); setReview({ ...review, status: "sending", statusMessage: undefined }); try { setReview(await onSend(review.id, review.revision)); } catch (reason) { setReview(review); setError(reason instanceof Error ? reason.message : String(reason)); } finally { setBusy(false); } }}>确认发送</button>
      <button type="button" className="secondary" disabled={busy || !editable} onClick={async () => { try { await onCancel?.(review.id); setReview({ ...review, status: "cancelled", statusMessage: undefined }); } catch (reason) { setError(reason instanceof Error ? reason.message : String(reason)); } }}>取消</button>
    </div>
    {!onSend && <p className="email-note">发送适配器尚未启用；当前只能审核，不能产生网络发送。</p>}
  </section>;
}
