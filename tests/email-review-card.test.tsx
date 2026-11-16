import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { EmailReviewCard, type EmailDraftReview } from "../src/features/email/EmailReviewCard";

const waiting: EmailDraftReview = {
  id: "email-1",
  accountId: 2,
  accountLabel: "lcr20041104a@gmail.com",
  recipients: ["recipient@example.com"],
  subject: "测试邮件",
  body: "测试正文",
  attachmentLabels: [],
  revision: 1,
  status: "waiting_for_user",
};

describe("EmailReviewCard", () => {
  it("shows the provider-confirmed sent result after the user clicks send", async () => {
    const sent = {
      ...waiting,
      status: "sent" as const,
      statusMessage: "邮箱服务已确认接收这封邮件。",
    };
    const onSend = vi.fn().mockResolvedValue(sent);
    render(
      <EmailReviewCard
        reviewId={waiting.id}
        onLoad={vi.fn().mockResolvedValue(waiting)}
        onSend={onSend}
      />,
    );

    fireEvent.click(await screen.findByRole("button", { name: "确认发送" }));

    await waitFor(() => expect(onSend).toHaveBeenCalledWith("email-1", 1));
    expect(await screen.findByText("邮箱服务已确认接收这封邮件。")).toBeVisible();
    expect(screen.getByRole("button", { name: "确认发送" })).toBeDisabled();
    expect(screen.getByLabelText("主题")).toBeDisabled();
  });

  it("applies an asynchronous send result event to the visible card", async () => {
    const { rerender } = render(
      <EmailReviewCard reviewId={waiting.id} onLoad={vi.fn().mockResolvedValue(waiting)} />,
    );
    await screen.findByDisplayValue("测试邮件");

    rerender(
      <EmailReviewCard
        reviewId={waiting.id}
        result={{ ...waiting, status: "unknown", statusMessage: "连接中断，发送结果未知。" }}
        onLoad={vi.fn().mockResolvedValue(waiting)}
      />,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent("连接中断，发送结果未知。");
  });
});
