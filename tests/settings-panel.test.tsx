import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SettingsPanel } from "../src/features/settings/SettingsPanel";

describe("SettingsPanel", () => {

  it("uses an application-preferences card instead of a bare settings form", async () => {
    render(<SettingsPanel onLoad={vi.fn().mockResolvedValue({ autostart: true, city: "上海" })} />);

    expect(await screen.findByRole("heading", { name: "应用偏好" })).toBeVisible();
    expect(screen.getByLabelText("开机自启").closest(".app-preferences-row")).not.toBeNull();
  });
  it("saves autostart immediately on toggle and city on blur", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(
      <SettingsPanel
        onLoad={vi.fn().mockResolvedValue({ autostart: true, city: "上海" })}
        onSave={onSave}
      />,
    );

    expect(await screen.findByLabelText("开机自启")).toBeChecked();
    expect(screen.getByLabelText("默认城市")).toHaveValue("上海");

    fireEvent.click(screen.getByLabelText("开机自启"));
    await waitFor(() =>
      expect(onSave).toHaveBeenCalledWith({ autostart: false, city: "上海" }),
    );

    fireEvent.change(screen.getByLabelText("默认城市"), { target: { value: "北京" } });
    fireEvent.blur(screen.getByLabelText("默认城市"));

    await waitFor(() =>
      expect(onSave).toHaveBeenLastCalledWith({ autostart: false, city: "北京" }),
    );
  });
});
