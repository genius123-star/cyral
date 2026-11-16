import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ChatComposer } from "../src/features/chat/ChatComposer";

describe("ChatComposer", () => {
  it("makes the non-interactive shell a native window drag region", () => {
    render(<ChatComposer onClose={vi.fn()} onSend={vi.fn()} />);

    expect(screen.getByRole("form")).toHaveAttribute("data-tauri-drag-region");
  });

  it("submits trimmed text and clears the input", async () => {
    const onSend = vi.fn();
    render(<ChatComposer onClose={vi.fn()} onSend={onSend} />);

    const input = screen.getByRole("textbox", { name: "给希莉尔发消息" });
    fireEvent.change(input, { target: { value: "  今天过得怎么样？  " } });
    fireEvent.submit(input.closest("form")!);

    expect(onSend).toHaveBeenCalledWith("今天过得怎么样？");
    await waitFor(() => expect(input).toHaveValue(""));
  });

  it("closes when Escape is pressed", () => {
    const onClose = vi.fn();
    render(<ChatComposer onClose={onClose} onSend={vi.fn()} />);

    fireEvent.keyDown(window, { key: "Escape" });

    expect(onClose).toHaveBeenCalledOnce();
  });

  it("keeps the message and shows the provider error when sending fails", async () => {
    render(
      <ChatComposer
        onClose={vi.fn()}
        onSend={() => Promise.reject(new Error("API Key 无效"))}
      />,
    );

    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "你好，希莉尔" } });
    fireEvent.submit(input.closest("form")!);

    expect(await screen.findByRole("alert")).toHaveTextContent("API Key 无效");
    expect(input).toHaveValue("你好，希莉尔");
  });

  it("shows staged files and sends only their candidate ids with the instruction", async () => {
    const onSend = vi.fn();
    render(
      <ChatComposer
        onClose={vi.fn()}
        onSend={onSend}
        droppedFiles={[{ candidateId: "drop-7", name: "课程作业.pdf", sizeBytes: 2048 }]}
      />,
    );

    expect(screen.getByText("课程作业.pdf")).toBeInTheDocument();
    const input = screen.getByRole("textbox");
    fireEvent.change(input, { target: { value: "把它作为附件发给老师" } });
    fireEvent.submit(input.closest("form")!);

    expect(onSend).toHaveBeenCalledWith("把它作为附件发给老师", ["drop-7"]);
  });

  it("removes a staged file without sending it", () => {
    const onRemoveFile = vi.fn();
    render(
      <ChatComposer
        onClose={vi.fn()}
        onSend={vi.fn()}
        onRemoveFile={onRemoveFile}
        droppedFiles={[{ candidateId: "drop-9", name: "简历.docx", sizeBytes: 512 }]}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "移除 简历.docx" }));
    expect(onRemoveFile).toHaveBeenCalledWith("drop-9");
  });
});
