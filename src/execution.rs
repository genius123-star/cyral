use std::error::Error;
use std::fmt::{Display, Formatter};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionOrigin {
    UserChat,
    Proactive,
    Background,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopAction {
    OpenInstalledApp,
    OpenAuthorizedFile,
    SendEmail,
    StartPlayback,
    OverwriteFile,
    DeleteData,
    InstallSoftware,
    RunCommand,
    SubmitForm,
    MakePayment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActionPolicy {
    Automatic,
    PrepareOnly,
    Forbidden,
}

impl DesktopAction {
    pub fn policy(self) -> ActionPolicy {
        match self {
            Self::OpenInstalledApp | Self::OpenAuthorizedFile => ActionPolicy::Automatic,
            Self::SendEmail => ActionPolicy::PrepareOnly,
            Self::StartPlayback
            | Self::OverwriteFile
            | Self::DeleteData
            | Self::InstallSoftware
            | Self::RunCommand
            | Self::SubmitForm
            | Self::MakePayment => ActionPolicy::Forbidden,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionStatus {
    Confirming,
    Running,
    WaitingForUser,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExecutionError {
    UserInitiationRequired,
    ExplicitRequestRequired,
    UserDecisionRequired,
    Cancelled,
    InvalidTransition,
    PreparationRequired,
}

impl Display for ExecutionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserInitiationRequired => formatter.write_str("桌面执行只能由用户明确发起"),
            Self::ExplicitRequestRequired => formatter.write_str("用户没有明确要求执行该桌面动作"),
            Self::UserDecisionRequired => formatter.write_str("该动作必须由用户亲自完成"),
            Self::Cancelled => formatter.write_str("任务已取消，不能继续执行"),
            Self::InvalidTransition => formatter.write_str("任务状态不允许继续操作"),
            Self::PreparationRequired => formatter.write_str("该任务必须停下并等待用户操作"),
        }
    }
}

impl Error for ExecutionError {}

#[derive(Debug, Eq, PartialEq)]
pub struct ExecutionTask {
    action: DesktopAction,
    status: ExecutionStatus,
}

impl ExecutionTask {
    pub fn new(origin: ExecutionOrigin, action: DesktopAction) -> Result<Self, ExecutionError> {
        if origin != ExecutionOrigin::UserChat {
            return Err(ExecutionError::UserInitiationRequired);
        }
        if action.policy() == ActionPolicy::Forbidden {
            return Err(ExecutionError::UserDecisionRequired);
        }
        Ok(Self {
            action,
            status: ExecutionStatus::Confirming,
        })
    }

    pub fn new_for_user_request(
        origin: ExecutionOrigin,
        action: DesktopAction,
        user_message: &str,
    ) -> Result<Self, ExecutionError> {
        let task = Self::new(origin, action)?;
        if matches!(
            action,
            DesktopAction::OpenInstalledApp | DesktopAction::OpenAuthorizedFile
        ) && !explicit_open_request(user_message)
        {
            return Err(ExecutionError::ExplicitRequestRequired);
        }
        Ok(task)
    }

    pub fn status(&self) -> ExecutionStatus {
        self.status
    }

    pub fn start(&mut self) -> Result<(), ExecutionError> {
        self.transition_from(ExecutionStatus::Confirming, ExecutionStatus::Running)
    }

    pub fn wait_for_user(&mut self) -> Result<(), ExecutionError> {
        if self.action.policy() != ActionPolicy::PrepareOnly {
            return Err(ExecutionError::PreparationRequired);
        }
        self.transition_from(ExecutionStatus::Running, ExecutionStatus::WaitingForUser)
    }

    pub fn complete(&mut self) -> Result<(), ExecutionError> {
        self.transition_from(ExecutionStatus::Running, ExecutionStatus::Completed)
    }

    pub fn fail(&mut self) -> Result<(), ExecutionError> {
        self.transition_from(ExecutionStatus::Running, ExecutionStatus::Failed)
    }

    pub fn cancel(&mut self) -> Result<(), ExecutionError> {
        match self.status {
            ExecutionStatus::Confirming
            | ExecutionStatus::Running
            | ExecutionStatus::WaitingForUser => {
                self.status = ExecutionStatus::Cancelled;
                Ok(())
            }
            ExecutionStatus::Cancelled => Err(ExecutionError::Cancelled),
            ExecutionStatus::Completed | ExecutionStatus::Failed => {
                Err(ExecutionError::InvalidTransition)
            }
        }
    }

    fn transition_from(
        &mut self,
        expected: ExecutionStatus,
        next: ExecutionStatus,
    ) -> Result<(), ExecutionError> {
        if self.status == ExecutionStatus::Cancelled {
            return Err(ExecutionError::Cancelled);
        }
        if self.status != expected {
            return Err(ExecutionError::InvalidTransition);
        }
        self.status = next;
        Ok(())
    }
}

fn explicit_open_request(user_message: &str) -> bool {
    let message = user_message.trim().to_lowercase();
    if message.is_empty() {
        return false;
    }
    let rejected = [
        "不要打开",
        "别打开",
        "不用打开",
        "无需打开",
        "不许打开",
        "打不开",
        "没有打开",
        "怎么打开",
        "如何打开",
        "不要启动",
        "别启动",
        "不用启动",
        "无法启动",
        "启动不了",
        "don't open",
        "do not open",
        "can't open",
        "cannot open",
        "how to open",
    ];
    if rejected.iter().any(|phrase| message.contains(phrase)) {
        return false;
    }
    [
        "打开", "启动", "运行", "唤起", "前往", "open ", "launch ", "start ",
    ]
    .iter()
    .any(|verb| message.contains(verb))
}

#[cfg(test)]
mod tests {
    use super::{ActionPolicy, DesktopAction, ExecutionOrigin, ExecutionStatus, ExecutionTask};

    #[test]
    fn only_user_chat_can_start_desktop_execution() {
        assert!(
            ExecutionTask::new(ExecutionOrigin::UserChat, DesktopAction::OpenInstalledApp,).is_ok()
        );

        for origin in [ExecutionOrigin::Proactive, ExecutionOrigin::Background] {
            let error = ExecutionTask::new(origin, DesktopAction::OpenInstalledApp)
                .expect_err("非用户对话来源必须被拒绝");
            assert_eq!(error.to_string(), "桌面执行只能由用户明确发起");
        }
    }

    #[test]
    fn decisive_actions_are_rejected_before_execution() {
        for action in [
            DesktopAction::StartPlayback,
            DesktopAction::OverwriteFile,
            DesktopAction::DeleteData,
            DesktopAction::InstallSoftware,
            DesktopAction::RunCommand,
            DesktopAction::SubmitForm,
            DesktopAction::MakePayment,
        ] {
            assert_eq!(action.policy(), ActionPolicy::Forbidden);
            let error = ExecutionTask::new(ExecutionOrigin::UserChat, action)
                .expect_err("决定性动作不得进入执行任务");
            assert_eq!(error.to_string(), "该动作必须由用户亲自完成");
        }
        assert_eq!(DesktopAction::SendEmail.policy(), ActionPolicy::PrepareOnly);
        let mut email = ExecutionTask::new(ExecutionOrigin::UserChat, DesktopAction::SendEmail)
            .expect("邮件只能进入准备态，不能直接发送");
        email.start().unwrap();
        email.wait_for_user().unwrap();
        assert_eq!(email.status(), ExecutionStatus::WaitingForUser);
    }

    #[test]
    fn cancelled_task_cannot_advance() {
        let mut task =
            ExecutionTask::new(ExecutionOrigin::UserChat, DesktopAction::OpenAuthorizedFile)
                .expect("授权文件打开应被允许");

        task.start().expect("任务应能开始");
        task.cancel().expect("运行中的任务应能取消");
        assert_eq!(task.status(), ExecutionStatus::Cancelled);

        let error = task.complete().expect_err("已取消任务不得继续进入完成状态");
        assert_eq!(error.to_string(), "任务已取消，不能继续执行");
    }

    #[test]
    fn opening_requires_an_explicit_non_negated_user_request() {
        assert!(ExecutionTask::new_for_user_request(
            ExecutionOrigin::UserChat,
            DesktopAction::OpenInstalledApp,
            "帮我打开微信",
        )
        .is_ok());
        assert!(ExecutionTask::new_for_user_request(
            ExecutionOrigin::UserChat,
            DesktopAction::OpenAuthorizedFile,
            "请把课程作业 PPT 打开",
        )
        .is_ok());

        for message in [
            "微信在哪里？",
            "不要打开微信",
            "微信打不开怎么办？",
            "不用启动 Chrome",
        ] {
            let error = ExecutionTask::new_for_user_request(
                ExecutionOrigin::UserChat,
                DesktopAction::OpenInstalledApp,
                message,
            )
            .expect_err("非明确或被否定的请求不得执行");
            assert_eq!(error.to_string(), "用户没有明确要求执行该桌面动作");
        }
    }
}
