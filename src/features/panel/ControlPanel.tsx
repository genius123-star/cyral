import { useEffect, useRef, useState } from "react";
import {
  ModelSettings,
  type ModelProviderInput,
  type SavedModelProvider,
} from "../settings/ModelSettings";
import {
  ConversationHistory,
  type ConversationMessage,
} from "../chat/ConversationHistory";
import { MemoryLibrary, type MemoryAtom } from "../organizer/MemoryLibrary";
import { TodoList, type TodoConfirmation, type TodoItem } from "../organizer/TodoList";
import { PrivacySettings, type PrivacyConfig } from "../settings/PrivacySettings";
import { ActivityTimeline, type ActivityTheme, type CyralActivityEvent } from "../companion/ActivityTimeline";
import { VisionSettings, type VisionProviderInput, type SavedVisionProvider } from "../settings/VisionSettings";
import { ModeStatus, type ModeState } from "../companion/ModeStatus";
import { SettingsPanel, type AppSettings } from "../settings/SettingsPanel";
import { UsagePanel, type MonthlyUsage, type UsagePoint } from "./UsagePanel";
import { EmailSettings, type EmailAccountInput, type EmailAccountView } from "../email/EmailSettings";
import { EmailReviewCard, type EmailDraftReview } from "../email/EmailReviewCard";

interface ControlPanelProps {
  onClose: () => void;
  onSaveModel?: (settings: ModelProviderInput) => unknown | Promise<unknown>;
  onLoadModel?: () => Promise<SavedModelProvider | null>;
  onLoadMessages?: () => Promise<ConversationMessage[]>;
  onLoadMemories?: () => Promise<MemoryAtom[]>;
  onUpdateMemory?: (id: number, content: string) => Promise<unknown>;
  onDeleteMemory?: (id: number) => Promise<unknown>;
  onLoadTodos?: () => Promise<TodoItem[]>;
  onConfirmTodo?: (todo: TodoConfirmation) => Promise<unknown>;
  onDiscardTodo?: (id: number) => Promise<unknown>;
  onCreateTodo?: (title: string, deadlineLocal: string) => Promise<unknown>;
  onCompleteTodo?: (id: number) => Promise<unknown>;
  onLoadPrivacy?: () => Promise<PrivacyConfig>;
  onSavePrivacy?: (config: PrivacyConfig) => Promise<PrivacyConfig>;
  onLoadActivity?: () => Promise<CyralActivityEvent[]>;
  onLoadThemes?: () => Promise<ActivityTheme[]>;
  onMuteTheme?: (identityKey: string) => Promise<unknown>;
  onDeleteTheme?: (identityKey: string) => Promise<unknown>;
  onLoadMode?: () => Promise<ModeState>;
  onToggleCompanion?: (enabled: boolean) => Promise<string | undefined>;
  onSaveVision?: (settings: VisionProviderInput) => Promise<unknown>;
  onLoadVision?: () => Promise<SavedVisionProvider | null>;
  onLoadSettings?: () => Promise<AppSettings>;
  onSaveSettings?: (settings: AppSettings) => Promise<unknown>;
  onLoadEmailAccounts?: () => Promise<EmailAccountView[]>;
  onSaveEmailAccount?: (account: EmailAccountInput, credential?: string) => Promise<EmailAccountView>;
  onDeleteEmailAccount?: (id: number) => Promise<unknown>;
  onConnectGmail?: (id: number) => Promise<EmailAccountView>;
  onDisconnectGmail?: (id: number) => Promise<EmailAccountView>;
  onTestEmailConnection?: (id: number) => Promise<string>;
  onLoadEmailDraft?: (id: string) => Promise<EmailDraftReview>;
  onUpdateEmailDraft?: (review: EmailDraftReview) => Promise<EmailDraftReview>;
  onCancelEmailDraft?: (id: string) => Promise<unknown>;
  onSendEmailDraft?: (id: string, revision: number) => Promise<EmailDraftReview>;
  onLoadUsage?: () => Promise<MonthlyUsage>;
  onLoadUsageHistory?: () => Promise<UsagePoint[]>;
  todoReviewId?: number;
  emailReviewId?: string;
  emailReviewResult?: EmailDraftReview;
}

const sections = ["模型配置", "对话记录", "功能模式", "隐私设置", "邮件助手", "活动用量", "待办提醒"] as const;
type Section = (typeof sections)[number];
type ModelSection = "模型连接" | "记忆库" | "用量预算";

const descriptions: Record<Section, { eyebrow: string; title: string; body: string }> = {
  对话记录: {
    eyebrow: "Conversation",
    title: "和希莉尔聊聊",
    body: "你们的对话记录只保存在这台电脑上。",
  },
 功能模式: {
    eyebrow: "Mode",
    title: "功能模式",
    body: "普通、陪伴与工作三种模式的开关状态。通过对话中的 /命令 切换。",
  },
  模型配置: {
    eyebrow: "Model Provider",
    title: "模型配置",
    body: "选择希莉尔使用的模型供应商、接口协议与预算上限。",
  },
  待办提醒: {
    eyebrow: "Reminders",
    title: "还没有待办",
    body: "以后你告诉我的事项，会先在这里确认时间和提前提醒节点。",
  },
  隐私设置: {
    eyebrow: "Privacy",
    title: "隐私授权",
    body: "集中管理屏幕观察、地理位置等隐私相关功能的授权。",
  },
  邮件助手: {
    eyebrow: "Mail",
    title: "邮件助手",
    body: "管理邮箱连接，并在发送前核对收件人、主题、正文与附件。最终发送始终由你确认。",
  },
  活动用量: {
    eyebrow: "Cyral Activity",
    title: "活动用量",
    body: "只记录希莉尔实际发生的观察与模型调用，不展示你的应用使用轨迹。",
  },
};

export function ControlPanel({
  onClose,
  onSaveModel,
  onLoadModel,
  onLoadMessages,
  onLoadMemories,
  onUpdateMemory,
  onDeleteMemory,
  onLoadTodos,
  onConfirmTodo,
  onDiscardTodo,
  onCreateTodo,
  onCompleteTodo,
  onLoadPrivacy,
  onSavePrivacy,
  onLoadActivity,
  onLoadThemes,
  onMuteTheme,
  onDeleteTheme,
  onLoadMode,
  onToggleCompanion,
  onSaveVision,
  onLoadVision,
  onLoadSettings,
  onSaveSettings,
  onLoadEmailAccounts,
  onSaveEmailAccount,
  onDeleteEmailAccount,
  onConnectGmail,
  onDisconnectGmail,
  onTestEmailConnection,
  onLoadEmailDraft,
  onUpdateEmailDraft,
  onCancelEmailDraft,
  onSendEmailDraft,
  onLoadUsage,
  onLoadUsageHistory,
  todoReviewId,
  emailReviewId,
  emailReviewResult,
}: ControlPanelProps) {
  const [activeSection, setActiveSection] = useState<Section>("模型配置");
  const [modelSection, setModelSection] = useState<ModelSection>("模型连接");
  const emailReviewRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (todoReviewId !== undefined) setActiveSection("待办提醒");
  }, [todoReviewId]);
  useEffect(() => {
    if (emailReviewId !== undefined) setActiveSection("邮件助手");
  }, [emailReviewId]);
  useEffect(() => {
    if (activeSection === "邮件助手" && emailReviewId !== undefined) {
      emailReviewRef.current?.scrollIntoView?.({ block: "start" });
    }
  }, [activeSection, emailReviewId, emailReviewResult]);
  const content = descriptions[activeSection];

  return (
    <main className="control-panel">
      <header className="panel-header" data-tauri-drag-region>
        <div>
          <span className="panel-kicker">CYRAL</span>
          <h1>希莉尔</h1>
        </div>
        <button type="button" aria-label="关闭侧边面板" onClick={onClose}>×</button>
      </header>

      <div className="panel-layout">
        <nav className="panel-tabs" role="tablist" aria-label="希莉尔功能">
          {sections.map((section) => (
            <button
              key={section}
              type="button"
              role="tab"
              aria-selected={activeSection === section}
              aria-controls="active-panel"
              onClick={() => setActiveSection(section)}
            >
              {section}
            </button>
          ))}
        </nav>

        <section
          id="active-panel"
          className="panel-content"
          role="tabpanel"
          aria-label={activeSection}
        >
          <span className="content-eyebrow">{content.eyebrow}</span>
          <h2>{content.title}</h2>
          <p>{content.body}</p>
          {activeSection === "对话记录" && (
            <ConversationHistory onLoad={onLoadMessages} />
          )}
          {activeSection === "待办提醒" && (
            <TodoList
              onLoad={onLoadTodos}
              onConfirm={onConfirmTodo}
              onDiscard={onDiscardTodo}
              onCreate={onCreateTodo}
              onComplete={onCompleteTodo}
              reviewTodoId={todoReviewId}
            />
          )}
          {activeSection === "隐私设置" && (
            <div className="privacy-center">
              <PrivacySettings onLoad={onLoadPrivacy} onSave={onSavePrivacy}>
                <SettingsPanel onLoad={onLoadSettings} onSave={onSaveSettings} />
              </PrivacySettings>
            </div>
          )}
          {activeSection === "邮件助手" && (
            <div className="email-center">
              {emailReviewId !== undefined && (
                <div ref={emailReviewRef}>
                  <EmailReviewCard reviewId={emailReviewId} result={emailReviewResult} onLoad={onLoadEmailDraft} onUpdate={onUpdateEmailDraft} onCancel={onCancelEmailDraft} onSend={onSendEmailDraft} />
                </div>
              )}
              <EmailSettings onLoad={onLoadEmailAccounts} onSave={onSaveEmailAccount} onDelete={onDeleteEmailAccount} onConnectGmail={onConnectGmail} onDisconnectGmail={onDisconnectGmail} onTestConnection={onTestEmailConnection} />
            </div>
          )}
          {activeSection === "活动用量" && (
            <div className="activity-usage-center">
              <UsagePanel onLoadUsage={onLoadUsage} onLoadHistory={onLoadUsageHistory} />
              <ActivityTimeline onLoad={onLoadActivity} onLoadThemes={onLoadThemes} onMuteTheme={onMuteTheme} onDeleteTheme={onDeleteTheme} />
            </div>
          )}
          {activeSection === "功能模式" && (
            <ModeStatus onLoad={onLoadMode} onToggleCompanion={onToggleCompanion} />
          )}
          <div hidden={activeSection !== "模型配置"} className="model-settings-area">
            <div className="panel-subtabs" aria-label="模型配置分类">
              {(["模型连接", "记忆库", "用量预算"] as const).map((section) => (
                <button
                  key={section}
                  type="button"
                  aria-pressed={modelSection === section}
                  onClick={() => setModelSection(section)}
                >
                  {section}
                </button>
              ))}
            </div>
            <div hidden={modelSection !== "模型连接"}>
              <ModelSettings onSave={onSaveModel ?? (() => {})} onLoad={onLoadModel} />
              {onSaveVision && onLoadVision && (
                <VisionSettings onSave={onSaveVision} onLoad={onLoadVision} />
              )}
            </div>
            <div hidden={modelSection !== "记忆库"}>
              <MemoryLibrary
                onLoad={onLoadMemories}
                onUpdate={onUpdateMemory}
                onDelete={onDeleteMemory}
              />
            </div>
            <div hidden={modelSection !== "用量预算"}>
              <UsagePanel onLoadUsage={onLoadUsage} onLoadHistory={onLoadUsageHistory} />
            </div>
          </div>
        </section>
      </div>
    </main>
  );
}
