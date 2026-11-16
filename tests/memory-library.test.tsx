import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { MemoryLibrary } from "../src/features/organizer/MemoryLibrary";

const memory = {
  id: 7,
  kind: "preference",
  content: "用户喜欢安静的写作环境",
  sourceExcerpt: "我写论文时喜欢安静一点",
  confidence: 0.95,
  createdAt: 1_800_000_000,
};

describe("MemoryLibrary", () => {
  it("shows a memory with its source and confidence", async () => {
    render(<MemoryLibrary onLoad={vi.fn().mockResolvedValue([memory])} />);

    expect(await screen.findByText(memory.content)).toBeVisible();
    expect(screen.getByText(/我写论文时喜欢安静一点/)).toBeVisible();
    expect(screen.getByText("95%" )).toBeVisible();
  });

  it("allows the user to edit and delete a memory", async () => {
    const onUpdate = vi.fn().mockResolvedValue(undefined);
    const onDelete = vi.fn().mockResolvedValue(undefined);
    render(
      <MemoryLibrary
        onLoad={vi.fn().mockResolvedValue([memory])}
        onUpdate={onUpdate}
        onDelete={onDelete}
      />,
    );
    await screen.findByText(memory.content);
    fireEvent.click(screen.getByRole("button", { name: "编辑记忆" }));
    fireEvent.change(screen.getByRole("textbox", { name: "记忆内容" }), {
      target: { value: "用户喜欢安静地写论文" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存记忆" }));
    await waitFor(() => expect(onUpdate).toHaveBeenCalledWith(7, "用户喜欢安静地写论文"));

    fireEvent.click(screen.getByRole("button", { name: "删除记忆" }));
    await waitFor(() => expect(onDelete).toHaveBeenCalledWith(7));
  });
});
