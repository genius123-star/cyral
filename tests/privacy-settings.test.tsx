import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PrivacySettings } from "../src/features/settings/PrivacySettings";

describe("PrivacySettings", () => {
  const initial = {
    screenObservation: false,
    autoLocation: false,
    proactiveInteraction: true,
    fileSearchDesktop: false,
    fileSearchDocuments: false,
    fileSearchDownloads: false,
  };

  it("persists a directory permission immediately and restores it after remount", async () => {
    let stored = initial;
    const onLoad = vi.fn(async () => stored);
    const onSave = vi.fn(async (next: typeof initial) => {
      stored = next;
      return stored;
    });
    const first = render(<PrivacySettings onLoad={onLoad} onSave={onSave} />);

    fireEvent.click(await screen.findByRole("checkbox", { name: "搜索桌面文件" }));
    await waitFor(() => expect(onSave).toHaveBeenCalledWith({
      ...initial,
      fileSearchDesktop: true,
    }));
    expect(await screen.findByText("桌面目录已授权")).toBeVisible();

    first.unmount();
    render(<PrivacySettings onLoad={onLoad} onSave={onSave} />);
    expect(await screen.findByRole("checkbox", { name: "搜索桌面文件" })).toBeChecked();
  });

  it("requires explicit opt-in before enabling desktop file search", async () => {
    const onSave = vi.fn().mockResolvedValue({ ...initial, fileSearchDesktop: true });
    render(<PrivacySettings onLoad={vi.fn().mockResolvedValue(initial)} onSave={onSave} />);

    fireEvent.click(await screen.findByRole("checkbox", { name: "搜索桌面文件" }));

    await waitFor(() => expect(onSave).toHaveBeenCalledWith({
      ...initial,
      fileSearchDesktop: true,
    }));
    expect(screen.getByText(/文件名和相对路径可能发送给已配置的模型供应商/)).toBeVisible();
  });

  it("keeps each file-search permission in its own aligned row", async () => {
    render(<PrivacySettings onLoad={vi.fn().mockResolvedValue(initial)} />);

    const desktopPermission = await screen.findByRole("checkbox", { name: "搜索桌面文件" });
    expect(desktopPermission.closest(".file-search-row")).not.toBeNull();
    expect(screen.getByText("本地文件搜索")).toBeVisible();
  });

  it("rolls a permission switch back when immediate persistence fails", async () => {
    render(
      <PrivacySettings
        onLoad={vi.fn().mockResolvedValue(initial)}
        onSave={vi.fn().mockRejectedValue(new Error("disk unavailable"))}
      />,
    );

    const documents = await screen.findByRole("checkbox", { name: "搜索文档文件" });
    fireEvent.click(documents);
    expect(await screen.findByText("文档目录授权保存失败，已恢复原状态")).toBeVisible();
    expect(documents).not.toBeChecked();
  });
});