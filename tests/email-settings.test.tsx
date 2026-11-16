import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { EmailSettings, type EmailAccountView } from "../src/features/email/EmailSettings";

const existingGmail: EmailAccountView = {
  id: 2,
  provider: "gmail",
  accountAddress: "lcr20041104a@gmail.com",
  displayName: "",
  signature: "",
  smtpHost: "",
  smtpPort: 465,
  smtpTls: "implicit",
  imapHost: "",
  imapPort: 993,
  imapTls: "implicit",
  isDefault: true,
  inboxEnabled: true,
  inboxIntervalMinutes: 10,
  inboxDetailLevel: "count",
  enabled: true,
  credentialConfigured: false,
};

describe("EmailSettings", () => {
  it("selects the existing default account after loading instead of presenting a duplicate insert", async () => {
    const onSave = vi.fn().mockResolvedValue(existingGmail);
    render(
      <EmailSettings
        onLoad={vi.fn().mockResolvedValue([existingGmail])}
        onSave={onSave}
      />,
    );

    const address = await screen.findByLabelText("邮箱地址");
    expect(address).toHaveValue("lcr20041104a@gmail.com");
    fireEvent.click(screen.getByRole("button", { name: "保存账户" }));

    await waitFor(() => expect(onSave).toHaveBeenCalledOnce());
    expect(onSave.mock.calls[0][0]).toMatchObject({
      id: 2,
      provider: "gmail",
      accountAddress: "lcr20041104a@gmail.com",
    });
  });
});
