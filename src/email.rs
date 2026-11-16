use std::{
    collections::{hash_map::DefaultHasher, HashMap},
    hash::{Hash, Hasher},
    path::PathBuf,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::storage::EmailAccount;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EmailTaskStatus {
    Preparing,
    WaitingForUser,
    Sending,
    Sent,
    Failed,
    Unknown,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EmailDraftInput {
    pub account_id: i64,
    pub recipients: Vec<String>,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub attachment_candidate_ids: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailDraftReview {
    pub id: String,
    pub account_id: i64,
    pub account_label: String,
    pub recipients: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachment_labels: Vec<String>,
    pub revision: u64,
    pub status: EmailTaskStatus,
    pub status_message: Option<String>,
}

#[derive(Debug)]
struct EmailTask {
    review: EmailDraftReview,
    content_hash: u64,
    attachment_candidate_ids: Vec<String>,
    attachments: Vec<EmailAttachment>,
}

#[derive(Clone, Debug)]
pub struct EmailAttachment {
    pub path: PathBuf,
    pub authorized_root: PathBuf,
    pub label: String,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub sha256: [u8; 32],
}

pub fn attachment_sha256(path: &std::path::Path) -> Result<[u8; 32], String> {
    let bytes = std::fs::read(path).map_err(|_| "无法读取附件，请重新选择附件".to_string())?;
    Ok(Sha256::digest(bytes).into())
}

#[derive(Clone, Debug)]
pub struct EmailSendJob {
    pub review: EmailDraftReview,
    pub attachments: Vec<EmailAttachment>,
}

#[derive(Default)]
pub struct EmailTaskStore {
    next_id: u64,
    tasks: HashMap<String, EmailTask>,
}

pub(crate) fn valid_email_address(value: &str) -> bool {
    let value = value.trim();
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !value.chars().any(char::is_whitespace)
}

fn content_hash(input: &EmailDraftInput, attachment_labels: &[String]) -> u64 {
    let mut hasher = DefaultHasher::new();
    input.account_id.hash(&mut hasher);
    input.recipients.hash(&mut hasher);
    input.subject.hash(&mut hasher);
    input.body.hash(&mut hasher);
    input.attachment_candidate_ids.hash(&mut hasher);
    attachment_labels.hash(&mut hasher);
    hasher.finish()
}

fn validate_draft(input: &EmailDraftInput) -> Result<(), String> {
    if input.account_id <= 0 {
        return Err("请选择已连接的发件账户".to_string());
    }
    if input.recipients.is_empty() || input.recipients.len() > 20 {
        return Err("收件人数量必须在 1 到 20 之间".to_string());
    }
    if let Some(invalid) = input
        .recipients
        .iter()
        .find(|recipient| !valid_email_address(recipient))
    {
        return Err(format!("收件地址格式无效：{}", invalid.trim()));
    }
    if input.subject.trim().is_empty() || input.subject.chars().count() > 200 {
        return Err("邮件主题不能为空且不能超过 200 个字符".to_string());
    }
    if input.body.trim().is_empty() || input.body.chars().count() > 100_000 {
        return Err("邮件正文不能为空且不能超过 100000 个字符".to_string());
    }
    if input.attachment_candidate_ids.len() > 10 {
        return Err("附件不能超过 10 个".to_string());
    }
    Ok(())
}

impl EmailTaskStore {
    pub fn prepare(
        &mut self,
        input: EmailDraftInput,
        account_label: String,
        attachments: Vec<EmailAttachment>,
    ) -> Result<EmailDraftReview, String> {
        validate_draft(&input)?;
        let attachment_labels = attachments
            .iter()
            .map(|attachment| attachment.label.clone())
            .collect::<Vec<_>>();
        if input.attachment_candidate_ids.len() != attachment_labels.len() {
            return Err("附件候选与审核清单不一致".to_string());
        }
        self.next_id = self.next_id.saturating_add(1);
        let id = format!("email-{}", self.next_id);
        let hash = content_hash(&input, &attachment_labels);
        let review = EmailDraftReview {
            id: id.clone(),
            account_id: input.account_id,
            account_label,
            recipients: input
                .recipients
                .into_iter()
                .map(|recipient| recipient.trim().to_string())
                .collect(),
            subject: input.subject.trim().to_string(),
            body: input.body.trim().to_string(),
            attachment_labels,
            revision: 1,
            status: EmailTaskStatus::WaitingForUser,
            status_message: None,
        };
        self.tasks.insert(
            id,
            EmailTask {
                review: review.clone(),
                content_hash: hash,
                attachment_candidate_ids: input.attachment_candidate_ids,
                attachments,
            },
        );
        Ok(review)
    }

    pub fn get(&self, id: &str) -> Result<EmailDraftReview, String> {
        self.tasks
            .get(id)
            .map(|task| task.review.clone())
            .ok_or_else(|| "邮件审核任务不存在或已失效".to_string())
    }

    pub fn latest(&self) -> Result<EmailDraftReview, String> {
        if self.next_id == 0 {
            return Err("还没有准备或发送过邮件".to_string());
        }
        self.get(&format!("email-{}", self.next_id))
    }

    pub fn update(
        &mut self,
        id: &str,
        input: EmailDraftInput,
        attachment_labels: Vec<String>,
    ) -> Result<EmailDraftReview, String> {
        validate_draft(&input)?;
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| "邮件审核任务不存在或已失效".to_string())?;
        if task.review.status != EmailTaskStatus::WaitingForUser {
            return Err("当前邮件状态不允许修改".to_string());
        }
        if task.review.account_id != input.account_id {
            return Err("审核期间不能替换发件账户，请重新创建邮件".to_string());
        }
        task.review.recipients = input
            .recipients
            .iter()
            .map(|recipient| recipient.trim().to_string())
            .collect();
        task.review.subject = input.subject.trim().to_string();
        task.review.body = input.body.trim().to_string();
        task.review.attachment_labels = attachment_labels.clone();
        task.review.revision = task.review.revision.saturating_add(1);
        task.content_hash = content_hash(&input, &attachment_labels);
        task.attachment_candidate_ids = input.attachment_candidate_ids;
        task.attachments.clear();
        Ok(task.review.clone())
    }

    pub fn update_content(
        &mut self,
        id: &str,
        recipients: Vec<String>,
        subject: String,
        body: String,
    ) -> Result<EmailDraftReview, String> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| "邮件审核任务不存在或已失效".to_string())?;
        if task.review.status != EmailTaskStatus::WaitingForUser {
            return Err("当前邮件状态不允许修改".to_string());
        }
        let input = EmailDraftInput {
            account_id: task.review.account_id,
            recipients,
            subject,
            body,
            attachment_candidate_ids: task.attachment_candidate_ids.clone(),
        };
        validate_draft(&input)?;
        task.review.recipients = input
            .recipients
            .iter()
            .map(|recipient| recipient.trim().to_string())
            .collect();
        task.review.subject = input.subject.trim().to_string();
        task.review.body = input.body.trim().to_string();
        task.review.revision = task.review.revision.saturating_add(1);
        task.content_hash = content_hash(&input, &task.review.attachment_labels);
        Ok(task.review.clone())
    }

    pub fn begin_send(&mut self, id: &str, revision: u64) -> Result<EmailSendJob, String> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| "邮件审核任务不存在或已失效".to_string())?;
        if task.review.status != EmailTaskStatus::WaitingForUser {
            return Err("该邮件已发送、正在发送、已取消或已结束".to_string());
        }
        if task.review.revision != revision {
            return Err("邮件内容已经修改，请重新检查后再发送".to_string());
        }
        let current_hash = content_hash(
            &EmailDraftInput {
                account_id: task.review.account_id,
                recipients: task.review.recipients.clone(),
                subject: task.review.subject.clone(),
                body: task.review.body.clone(),
                attachment_candidate_ids: task.attachment_candidate_ids.clone(),
            },
            &task.review.attachment_labels,
        );
        if current_hash != task.content_hash {
            return Err("邮件审核内容校验失败，请重新创建邮件".to_string());
        }
        for attachment in &task.attachments {
            attachment.revalidate()?;
        }
        task.review.status = EmailTaskStatus::Sending;
        Ok(EmailSendJob {
            review: task.review.clone(),
            attachments: task.attachments.clone(),
        })
    }

    pub fn finish(
        &mut self,
        id: &str,
        status: EmailTaskStatus,
        status_message: Option<String>,
    ) -> Result<(), String> {
        if !matches!(
            status,
            EmailTaskStatus::Sent | EmailTaskStatus::Failed | EmailTaskStatus::Unknown
        ) {
            return Err("邮件发送只能进入 sent、failed 或 unknown 终态".to_string());
        }
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| "邮件审核任务不存在或已失效".to_string())?;
        if task.review.status != EmailTaskStatus::Sending {
            return Err("只有发送中的邮件才能写入发送结果".to_string());
        }
        task.review.status = status;
        task.review.status_message = status_message;
        Ok(())
    }

    pub fn cancel(&mut self, id: &str) -> Result<(), String> {
        let task = self
            .tasks
            .get_mut(id)
            .ok_or_else(|| "邮件审核任务不存在或已失效".to_string())?;
        if !matches!(
            task.review.status,
            EmailTaskStatus::Preparing | EmailTaskStatus::WaitingForUser
        ) {
            return Err("该邮件已经进入发送或终态，不能取消".to_string());
        }
        task.review.status = EmailTaskStatus::Cancelled;
        Ok(())
    }
}

impl EmailAttachment {
    fn revalidate(&self) -> Result<(), String> {
        let root = self
            .authorized_root
            .canonicalize()
            .map_err(|_| "附件授权目录已失效，请重新选择附件".to_string())?;
        let path = self
            .path
            .canonicalize()
            .map_err(|_| "附件已不存在，请重新选择附件".to_string())?;
        let metadata = path
            .metadata()
            .map_err(|_| "无法读取附件，请重新选择附件".to_string())?;
        if !metadata.is_file() || !path.starts_with(root) {
            return Err("附件已离开授权目录，请重新选择附件".to_string());
        }
        if metadata.len() != self.size || attachment_sha256(&path)? != self.sha256 {
            return Err("附件在审核后发生变化，请重新准备邮件".to_string());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SendOutcome {
    Sent,
    Failed(String),
    Unknown(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InboxMessageMetadata {
    pub provider_message_id: String,
    pub sender: String,
    pub subject: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImapDelta {
    pub cursor: String,
    pub messages: Vec<InboxMessageMetadata>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GmailCredential {
    pub refresh_token: String,
    pub access_token: String,
    pub expires_at: i64,
}

fn build_message(
    account: &EmailAccount,
    review: &EmailDraftReview,
    attachments: &[EmailAttachment],
) -> Result<lettre::Message, String> {
    use lettre::message::{header::ContentType, Attachment, Mailbox, MultiPart, SinglePart};

    let address = account
        .account_address
        .parse()
        .map_err(|_| "发件账户地址无效".to_string())?;
    let from = Mailbox::new(
        (!account.display_name.trim().is_empty()).then(|| account.display_name.clone()),
        address,
    );
    let mut builder = lettre::Message::builder()
        .from(from)
        .subject(review.subject.clone());
    for recipient in &review.recipients {
        builder = builder.to(recipient
            .parse()
            .map_err(|_| format!("收件地址格式无效：{recipient}"))?);
    }
    if attachments.is_empty() {
        return builder
            .header(ContentType::TEXT_PLAIN)
            .body(review.body.clone())
            .map_err(|error| format!("无法生成 MIME 邮件：{error}"));
    }
    let mut multipart = MultiPart::mixed().singlepart(SinglePart::plain(review.body.clone()));
    let mut total_size = 0_u64;
    for attachment in attachments {
        attachment.revalidate()?;
        total_size = total_size.saturating_add(attachment.size);
        if total_size > 20 * 1024 * 1024 {
            return Err("附件总大小不能超过 20 MiB".to_string());
        }
        let bytes = std::fs::read(&attachment.path)
            .map_err(|_| format!("无法读取附件：{}", attachment.label))?;
        multipart = multipart.singlepart(
            Attachment::new(attachment.label.clone()).body(
                bytes,
                ContentType::parse("application/octet-stream")
                    .map_err(|_| "无法生成附件 MIME 类型".to_string())?,
            ),
        );
    }
    builder
        .multipart(multipart)
        .map_err(|error| format!("无法生成 MIME 邮件：{error}"))
}

pub fn send_smtp(
    account: &EmailAccount,
    credential: &str,
    review: &EmailDraftReview,
    attachments: &[EmailAttachment],
) -> SendOutcome {
    use lettre::{transport::smtp::authentication::Credentials, SmtpTransport, Transport};

    let message = match build_message(account, review, attachments) {
        Ok(message) => message,
        Err(error) => return SendOutcome::Failed(error),
    };
    let builder = if account.smtp_tls == "starttls" {
        SmtpTransport::starttls_relay(&account.smtp_host)
    } else {
        SmtpTransport::relay(&account.smtp_host)
    };
    let transport = match builder {
        Ok(builder) => builder
            .port(account.smtp_port as u16)
            .credentials(Credentials::new(
                account.account_address.clone(),
                credential.to_string(),
            ))
            .timeout(Some(std::time::Duration::from_secs(30)))
            .build(),
        Err(error) => return SendOutcome::Failed(format!("SMTP TLS 配置无效：{error}")),
    };
    match transport.send(&message) {
        Ok(_) => SendOutcome::Sent,
        Err(error) if error.is_timeout() || error.is_transport_shutdown() => {
            SendOutcome::Unknown("连接在确认服务端结果前中断；不会自动重试".to_string())
        }
        Err(error) if error.is_transient() => {
            SendOutcome::Failed("邮件服务器暂时拒绝请求；未自动重试".to_string())
        }
        Err(error) if error.is_permanent() || error.is_response() => {
            SendOutcome::Failed("邮件服务器拒绝发送，请检查账户、收件人或服务商限制".to_string())
        }
        Err(_) => SendOutcome::Failed("SMTP 连接或认证失败".to_string()),
    }
}

fn parse_imap_cursor(cursor: &str) -> Option<(u32, u32)> {
    let (validity, uid) = cursor.split_once(':')?;
    Some((validity.parse().ok()?, uid.parse().ok()?))
}

fn decode_header_value(raw: &[u8], name: &str) -> String {
    let Ok((headers, _)) = mailparse::parse_headers(raw) else {
        return String::new();
    };
    headers
        .iter()
        .find(|header| header.get_key_ref().eq_ignore_ascii_case(name))
        .map(|header| header.get_value())
        .unwrap_or_default()
}

pub async fn check_imap(
    account: &EmailAccount,
    credential: &str,
    previous_cursor: Option<&str>,
) -> Result<ImapDelta, String> {
    use futures_util::TryStreamExt;

    if account.imap_tls != "implicit" {
        return Err("首版 IMAP 只允许隐式 TLS，拒绝明文或未验证升级连接".to_string());
    }
    let tcp =
        tokio::net::TcpStream::connect((account.imap_host.as_str(), account.imap_port as u16))
            .await
            .map_err(|_| "无法连接 IMAP 服务器".to_string())?;
    let connector =
        native_tls::TlsConnector::new().map_err(|_| "无法初始化系统 TLS".to_string())?;
    let tls = tokio_native_tls::TlsConnector::from(connector)
        .connect(&account.imap_host, tcp)
        .await
        .map_err(|_| "IMAP TLS 握手失败".to_string())?;
    let client = async_imap::Client::new(tls);
    let mut session = client
        .login(&account.account_address, credential)
        .await
        .map_err(|_| "IMAP 认证失败".to_string())?;
    let mailbox = session
        .select("INBOX")
        .await
        .map_err(|_| "无法打开收件箱".to_string())?;
    let validity = mailbox
        .uid_validity
        .ok_or_else(|| "邮箱服务器未提供 UIDVALIDITY，无法安全增量检查".to_string())?;
    let newest_uid = mailbox.uid_next.unwrap_or(1).saturating_sub(1);
    let current_cursor = format!("{validity}:{newest_uid}");
    let Some((previous_validity, previous_uid)) = previous_cursor.and_then(parse_imap_cursor)
    else {
        let _ = session.logout().await;
        return Ok(ImapDelta {
            cursor: current_cursor,
            messages: Vec::new(),
        });
    };
    if previous_validity != validity || newest_uid <= previous_uid {
        let _ = session.logout().await;
        return Ok(ImapDelta {
            cursor: current_cursor,
            messages: Vec::new(),
        });
    }
    let first_uid = previous_uid.saturating_add(1);
    let first_uid = first_uid.max(newest_uid.saturating_sub(49));
    let mut fetches = session
        .uid_fetch(
            format!("{first_uid}:{newest_uid}"),
            "(UID BODY.PEEK[HEADER])",
        )
        .await
        .map_err(|_| "无法读取新邮件头".to_string())?;
    let mut messages = Vec::new();
    while let Some(fetch) = fetches
        .try_next()
        .await
        .map_err(|_| "读取新邮件头时连接中断".to_string())?
    {
        let Some(uid) = fetch.uid else { continue };
        let Some(header) = fetch.header() else {
            continue;
        };
        messages.push(InboxMessageMetadata {
            provider_message_id: format!("{validity}:{uid}"),
            sender: decode_header_value(header, "From"),
            subject: decode_header_value(header, "Subject"),
        });
    }
    drop(fetches);
    let _ = session.logout().await;
    messages.sort_by(|left, right| left.provider_message_id.cmp(&right.provider_message_id));
    Ok(ImapDelta {
        cursor: current_cursor,
        messages,
    })
}

pub async fn exchange_gmail_code(
    client: &reqwest::Client,
    client_id: &str,
    client_secret: Option<String>,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    now: i64,
) -> Result<GmailCredential, String> {
    let mut form = vec![
        ("client_id", client_id),
        ("code", code),
        ("code_verifier", verifier),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
    ];
    append_optional_client_secret(&mut form, client_secret.as_deref());
    let response = client
        .post("https://oauth2.googleapis.com/token")
        .form(&form)
        .send()
        .await
        .map_err(|_| "无法连接 Google OAuth 服务".to_string())?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Google OAuth 返回了无法解析的响应".to_string())?;
    if !status.is_success() {
        let code = body["error"].as_str().unwrap_or("unknown_error");
        let description = body["error_description"]
            .as_str()
            .unwrap_or("Google 未提供错误详情")
            .chars()
            .take(240)
            .collect::<String>();
        if code == "invalid_request" && description.contains("client_secret") {
            return Err(
                "Google OAuth 客户端要求 client_secret，但当前构建未配置 CYRAL_GOOGLE_CLIENT_SECRET"
                    .to_string(),
            );
        }
        return Err(format!(
            "Google OAuth 令牌交换失败（{code}）：{description}"
        ));
    }
    let refresh_token = body["refresh_token"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("Google 未返回刷新令牌，请撤销旧授权后重新连接")?;
    let access_token = body["access_token"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("Google 未返回访问令牌")?;
    let expires_in = body["expires_in"]
        .as_i64()
        .unwrap_or(3600)
        .clamp(60, 86_400);
    Ok(GmailCredential {
        refresh_token: refresh_token.to_string(),
        access_token: access_token.to_string(),
        expires_at: now.saturating_add(expires_in),
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GmailProfile {
    pub email_address: String,
    pub history_id: String,
}

/// 读取 OAuth 令牌实际对应的 Google 账户，并取得建立增量收件基线所需的 historyId。
pub async fn get_gmail_profile(
    client: &reqwest::Client,
    credential: &GmailCredential,
) -> Result<GmailProfile, String> {
    let response = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/profile")
        .bearer_auth(&credential.access_token)
        .send()
        .await
        .map_err(|_| "无法连接 Gmail".to_string())?;
    if !response.status().is_success() {
        return Err("Gmail 授权验证失败，请重新连接".to_string());
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Gmail 返回了无法解析的账户信息".to_string())?;
    let email_address = body["emailAddress"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("Gmail 未返回授权账户地址")?
        .to_string();
    let history_id = body["historyId"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("Gmail 未返回 historyId")?
        .to_string();
    Ok(GmailProfile {
        email_address,
        history_id,
    })
}

/// Gmail INBOX 标签给出的当前未读数量；不读取邮件正文。
pub async fn get_gmail_inbox_unread_count(
    client: &reqwest::Client,
    credential: &GmailCredential,
) -> Result<u64, String> {
    let response = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/labels/INBOX")
        .bearer_auth(&credential.access_token)
        .send()
        .await
        .map_err(|_| "无法连接 Gmail 收件箱".to_string())?;
    if !response.status().is_success() {
        return Err("Gmail 拒绝读取未读数量，请重新连接并确认收件权限".to_string());
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Gmail 返回了无法解析的收件箱状态".to_string())?;
    body["messagesUnread"]
        .as_u64()
        .ok_or_else(|| "Gmail 未返回收件箱未读数量".to_string())
}

pub async fn refresh_gmail_token(
    client: &reqwest::Client,
    client_id: &str,
    client_secret: Option<String>,
    credential: &mut GmailCredential,
    now: i64,
) -> Result<bool, String> {
    if credential.expires_at > now.saturating_add(60) && !credential.access_token.is_empty() {
        return Ok(false);
    }
    let mut form = vec![
        ("client_id", client_id),
        ("refresh_token", credential.refresh_token.as_str()),
        ("grant_type", "refresh_token"),
    ];
    append_optional_client_secret(&mut form, client_secret.as_deref());
    let response = client
        .post("https://oauth2.googleapis.com/token")
        .form(&form)
        .send()
        .await
        .map_err(|_| "无法刷新 Gmail 授权".to_string())?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Gmail 授权刷新响应无法解析".to_string())?;
    if !status.is_success() {
        if body["error"] == "invalid_request"
            && body["error_description"]
                .as_str()
                .is_some_and(|description| description.contains("client_secret"))
        {
            return Err(
                "Google OAuth 客户端要求 client_secret，但当前构建未配置 CYRAL_GOOGLE_CLIENT_SECRET"
                    .to_string(),
            );
        }
        return Err("Gmail 授权已失效，请重新连接".to_string());
    }
    credential.access_token = body["access_token"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or("Gmail 未返回新的访问令牌")?
        .to_string();
    credential.expires_at = now.saturating_add(
        body["expires_in"]
            .as_i64()
            .unwrap_or(3600)
            .clamp(60, 86_400),
    );
    Ok(true)
}

fn append_optional_client_secret<'a>(
    form: &mut Vec<(&'a str, &'a str)>,
    client_secret: Option<&'a str>,
) {
    if let Some(client_secret) = client_secret.filter(|value| !value.trim().is_empty()) {
        form.push(("client_secret", client_secret));
    }
}

pub async fn send_gmail(
    client: &reqwest::Client,
    account: &EmailAccount,
    credential: &GmailCredential,
    review: &EmailDraftReview,
    attachments: &[EmailAttachment],
) -> SendOutcome {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};

    let message = match build_message(account, review, attachments) {
        Ok(message) => message,
        Err(error) => return SendOutcome::Failed(error),
    };
    let raw = URL_SAFE_NO_PAD.encode(message.formatted());
    let response = client
        .post("https://gmail.googleapis.com/gmail/v1/users/me/messages/send")
        .bearer_auth(&credential.access_token)
        .json(&serde_json::json!({ "raw": raw }))
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => SendOutcome::Sent,
        Ok(response) if response.status().is_server_error() => {
            SendOutcome::Unknown("Gmail 在确认发送结果前返回服务端错误；不会自动重试".to_string())
        }
        Ok(_) => SendOutcome::Failed("Gmail 拒绝发送，请检查授权与邮件内容".to_string()),
        Err(error) if error.is_timeout() || error.is_connect() => {
            SendOutcome::Unknown("连接在确认 Gmail 结果前中断；不会自动重试".to_string())
        }
        Err(_) => SendOutcome::Failed("Gmail 请求失败".to_string()),
    }
}

pub async fn check_gmail(
    client: &reqwest::Client,
    credential: &GmailCredential,
    previous_cursor: Option<&str>,
) -> Result<ImapDelta, String> {
    let newest_history = get_gmail_profile(client, credential).await?.history_id;
    let Some(start_history) = previous_cursor.filter(|value| !value.is_empty()) else {
        return Ok(ImapDelta {
            cursor: newest_history,
            messages: Vec::new(),
        });
    };
    let response = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/history")
        .bearer_auth(&credential.access_token)
        .query(&[
            ("startHistoryId", start_history),
            ("historyTypes", "messageAdded"),
            ("labelId", "INBOX"),
            ("maxResults", "50"),
        ])
        .send()
        .await
        .map_err(|_| "Gmail 增量检查连接失败".to_string())?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(ImapDelta {
            cursor: newest_history,
            messages: Vec::new(),
        });
    }
    if !response.status().is_success() {
        return Err("Gmail 增量检查失败".to_string());
    }
    let history: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Gmail 历史响应无法解析".to_string())?;
    let mut ids = Vec::new();
    for record in history["history"].as_array().into_iter().flatten() {
        for added in record["messagesAdded"].as_array().into_iter().flatten() {
            if let Some(id) = added["message"]["id"].as_str() {
                if !ids.iter().any(|existing| existing == id) {
                    ids.push(id.to_string());
                }
            }
        }
    }
    ids.truncate(50);
    let mut messages = Vec::new();
    for id in ids {
        let response = client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}"
            ))
            .bearer_auth(&credential.access_token)
            .query(&[
                ("format", "metadata"),
                ("metadataHeaders", "From"),
                ("metadataHeaders", "Subject"),
            ])
            .send()
            .await
            .map_err(|_| "Gmail 邮件头读取失败".to_string())?;
        if !response.status().is_success() {
            continue;
        }
        let message: serde_json::Value = response
            .json()
            .await
            .map_err(|_| "Gmail 邮件头响应无法解析".to_string())?;
        let headers = message["payload"]["headers"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let value = |name: &str| {
            headers
                .iter()
                .find(|header| {
                    header["name"]
                        .as_str()
                        .is_some_and(|key| key.eq_ignore_ascii_case(name))
                })
                .and_then(|header| header["value"].as_str())
                .unwrap_or("")
                .to_string()
        };
        messages.push(InboxMessageMetadata {
            provider_message_id: id,
            sender: value("From"),
            subject: value("Subject"),
        });
    }
    Ok(ImapDelta {
        cursor: history["historyId"]
            .as_str()
            .unwrap_or(&newest_history)
            .to_string(),
        messages,
    })
}

/// 读取今天收件箱最近邮件的发件人和主题；不请求正文或附件。
pub async fn get_recent_gmail_metadata(
    client: &reqwest::Client,
    credential: &GmailCredential,
    day_start_unix: i64,
    limit: usize,
) -> Result<Vec<InboxMessageMetadata>, String> {
    let limit = limit.clamp(1, 5);
    let response = client
        .get("https://gmail.googleapis.com/gmail/v1/users/me/messages")
        .bearer_auth(&credential.access_token)
        .query(&[
            ("q", format!("in:inbox after:{day_start_unix}")),
            ("maxResults", limit.to_string()),
        ])
        .send()
        .await
        .map_err(|_| "无法连接 Gmail 收件箱".to_string())?;
    if !response.status().is_success() {
        return Err("Gmail 拒绝读取今日邮件元数据，请重新连接并确认收件权限".to_string());
    }
    let listing: serde_json::Value = response
        .json()
        .await
        .map_err(|_| "Gmail 返回了无法解析的邮件列表".to_string())?;
    let ids = listing["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["id"].as_str())
        .take(limit)
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut messages = Vec::with_capacity(ids.len());
    for id in ids {
        let response = client
            .get(format!(
                "https://gmail.googleapis.com/gmail/v1/users/me/messages/{id}"
            ))
            .bearer_auth(&credential.access_token)
            .query(&[
                ("format", "metadata"),
                ("metadataHeaders", "From"),
                ("metadataHeaders", "Subject"),
            ])
            .send()
            .await
            .map_err(|_| "Gmail 邮件头读取失败".to_string())?;
        if !response.status().is_success() {
            continue;
        }
        let message: serde_json::Value = response
            .json()
            .await
            .map_err(|_| "Gmail 邮件头响应无法解析".to_string())?;
        let headers = message["payload"]["headers"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let value = |name: &str| {
            headers
                .iter()
                .find(|header| {
                    header["name"]
                        .as_str()
                        .is_some_and(|key| key.eq_ignore_ascii_case(name))
                })
                .and_then(|header| header["value"].as_str())
                .unwrap_or("")
                .to_string()
        };
        messages.push(InboxMessageMetadata {
            provider_message_id: id,
            sender: value("From"),
            subject: value("Subject"),
        });
    }
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use super::{
        append_optional_client_secret, build_message, decode_header_value, parse_imap_cursor,
        EmailDraftInput, EmailTaskStatus, EmailTaskStore,
    };
    use crate::storage::EmailAccount;
    use mailparse::MailHeaderMap;

    fn input() -> EmailDraftInput {
        EmailDraftInput {
            account_id: 1,
            recipients: vec!["recipient@example.com".to_string()],
            subject: "项目进度".to_string(),
            body: "正文".to_string(),
            attachment_candidate_ids: vec![],
        }
    }

    #[test]
    fn oauth_form_includes_only_a_non_empty_client_secret() {
        let mut form = vec![("client_id", "desktop-client")];
        append_optional_client_secret(&mut form, Some("desktop-secret"));
        assert_eq!(form.last(), Some(&("client_secret", "desktop-secret")));

        let mut empty_form = vec![("client_id", "desktop-client")];
        append_optional_client_secret(&mut empty_form, Some("  "));
        assert_eq!(empty_form, vec![("client_id", "desktop-client")]);
    }

    fn account() -> EmailAccount {
        EmailAccount {
            id: 1,
            provider: "qq".to_string(),
            account_address: "sender@example.com".to_string(),
            display_name: "希莉尔测试".to_string(),
            signature: String::new(),
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 465,
            smtp_tls: "implicit".to_string(),
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            imap_tls: "implicit".to_string(),
            is_default: true,
            inbox_enabled: true,
            inbox_interval_minutes: 10,
            inbox_detail_level: "count".to_string(),
            enabled: true,
        }
    }

    #[test]
    fn mime_message_matches_the_reviewed_fields() {
        let mut store = EmailTaskStore::default();
        let review = store
            .prepare(input(), "sender@example.com".to_string(), vec![])
            .unwrap();
        let formatted = build_message(&account(), &review, &[]).unwrap().formatted();
        let mime = String::from_utf8(formatted.clone()).unwrap();
        let parsed = mailparse::parse_mail(&formatted).unwrap();
        assert!(mime.contains("To: recipient@example.com"));
        assert_eq!(
            parsed.headers.get_first_value("Subject").as_deref(),
            Some("项目进度")
        );
        assert_eq!(parsed.get_body().unwrap().trim(), "正文");
    }

    #[test]
    fn imap_cursor_rejects_malformed_values() {
        assert_eq!(parse_imap_cursor("81:902"), Some((81, 902)));
        assert_eq!(parse_imap_cursor("81"), None);
        assert_eq!(parse_imap_cursor("x:902"), None);
    }

    #[test]
    fn header_parser_reads_metadata_without_a_body() {
        let header = b"From: sender@example.com\r\nSubject: status update\r\n\r\n";
        assert_eq!(decode_header_value(header, "From"), "sender@example.com");
        assert_eq!(decode_header_value(header, "Subject"), "status update");
    }

    #[test]
    fn attachment_changed_after_review_cannot_be_sent() {
        let root = std::env::temp_dir().join(format!(
            "cyral-email-attachment-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("report.txt");
        std::fs::write(&path, "first").unwrap();
        let metadata = path.metadata().unwrap();
        let mut draft = input();
        draft.attachment_candidate_ids = vec!["file-1".to_string()];
        let mut store = EmailTaskStore::default();
        let review = store
            .prepare(
                draft,
                "sender@example.com".to_string(),
                vec![super::EmailAttachment {
                    sha256: super::attachment_sha256(&path).unwrap(),
                    path: path.clone(),
                    authorized_root: root.clone(),
                    label: "report.txt".to_string(),
                    size: metadata.len(),
                    modified: metadata.modified().ok(),
                }],
            )
            .unwrap();
        std::fs::write(&path, "other").unwrap();
        assert!(store.begin_send(&review.id, review.revision).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prepared_email_waits_for_the_user_and_rejects_duplicate_send() {
        let mut store = EmailTaskStore::default();
        let review = store
            .prepare(input(), "sender@example.com".to_string(), vec![])
            .unwrap();
        assert_eq!(review.status, EmailTaskStatus::WaitingForUser);
        store.begin_send(&review.id, review.revision).unwrap();
        assert!(store.begin_send(&review.id, review.revision).is_err());
    }

    #[test]
    fn latest_email_keeps_the_provider_confirmed_terminal_result() {
        let mut store = EmailTaskStore::default();
        let review = store
            .prepare(input(), "sender@example.com".to_string(), vec![])
            .unwrap();
        store.begin_send(&review.id, review.revision).unwrap();
        store
            .finish(
                &review.id,
                EmailTaskStatus::Sent,
                Some("邮箱服务已确认接收这封邮件。".to_string()),
            )
            .unwrap();

        let latest = store.latest().unwrap();
        assert_eq!(latest.status, EmailTaskStatus::Sent);
        assert_eq!(
            latest.status_message.as_deref(),
            Some("邮箱服务已确认接收这封邮件。")
        );
    }

    #[test]
    fn editing_invalidates_the_previous_revision() {
        let mut store = EmailTaskStore::default();
        let review = store
            .prepare(input(), "sender@example.com".to_string(), vec![])
            .unwrap();
        let mut edited = input();
        edited.subject = "更新后的主题".to_string();
        let updated = store.update(&review.id, edited, vec![]).unwrap();
        assert!(store.begin_send(&review.id, review.revision).is_err());
        assert!(store.begin_send(&review.id, updated.revision).is_ok());
    }

    #[test]
    fn cancelled_email_cannot_be_sent() {
        let mut store = EmailTaskStore::default();
        let review = store
            .prepare(input(), "sender@example.com".to_string(), vec![])
            .unwrap();
        store.cancel(&review.id).unwrap();
        assert!(store.begin_send(&review.id, review.revision).is_err());
    }

    #[test]
    fn recipient_must_be_an_explicit_email_address() {
        let mut store = EmailTaskStore::default();
        let mut invalid = input();
        invalid.recipients = vec!["张老师".to_string()];
        assert!(store
            .prepare(invalid, "sender@example.com".to_string(), vec![])
            .unwrap_err()
            .contains("格式无效"));
    }
}
