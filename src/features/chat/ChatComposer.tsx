import { useEffect, useState, type FormEvent } from "react";

interface ChatComposerProps {
  onClose: () => void;
  onSend: (message: string, candidateIds?: string[]) => unknown | Promise<unknown>;
  droppedFiles?: Array<{ candidateId: string; name: string; sizeBytes: number }>;
  externalError?: string;
  onRemoveFile?: (candidateId: string) => void;
}

export function ChatComposer({
  onClose,
  onSend,
  droppedFiles = [],
  externalError,
  onRemoveFile = () => {},
}: ChatComposerProps) {
  const [message, setMessage] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string>();

  useEffect(() => {
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const trimmedMessage = message.trim();
    if (!trimmedMessage || sending) return;
    setSending(true);
    setError(undefined);
    try {
      if (droppedFiles.length > 0) {
        await onSend(trimmedMessage, droppedFiles.map((file) => file.candidateId));
      } else {
        await onSend(trimmedMessage);
      }
      setMessage("");
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setSending(false);
    }
  };

  return (
    <main className="composer-surface">
      <form
        className={`composer${droppedFiles.length > 0 ? " composer-with-files" : ""}`}
        aria-label="希莉尔对话输入框"
        data-tauri-drag-region
        onSubmit={submit}
      >
        {droppedFiles.length > 0 && (
          <div className="composer-files" aria-label="已拖入文件">
            {droppedFiles.map((file) => (
              <span className="composer-file" key={file.candidateId}>
                <span title={file.name}>{file.name}</span>
                <small>{file.sizeBytes < 1024 ? `${file.sizeBytes} B` : `${(file.sizeBytes / 1024).toFixed(1)} KiB`}</small>
                <button type="button" aria-label={`移除 ${file.name}`} onClick={() => onRemoveFile(file.candidateId)}>×</button>
              </span>
            ))}
          </div>
        )}
        <div className="composer-row">
          <input
            aria-label="给希莉尔发消息"
            autoFocus
            placeholder={droppedFiles.length > 0 ? "告诉我如何处理这些文件…" : "想和我说什么？"}
            value={message}
            onChange={(event) => setMessage(event.target.value)}
          />
          <button type="button" aria-label="关闭输入框" onClick={onClose}>×</button>
          {(error || externalError) && <span className="composer-error" role="alert">{error ?? externalError}</span>}
          <button type="submit" aria-label={sending ? "希莉尔正在思考" : "发送消息"} disabled={sending}>
            {sending ? "…" : "↑"}
          </button>
        </div>
      </form>
    </main>
  );
}
