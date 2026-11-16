import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ModelSettings } from "../src/features/settings/ModelSettings";

describe("ModelSettings", () => {
  it("loads the persisted provider instead of overwriting it with defaults", async () => {
    render(
      <ModelSettings
        onSave={vi.fn()}
        onLoad={() => Promise.resolve({
          providerName: "我的中转站",
          baseUrl: "https://api3.gemai.cc",
          protocol: "responses",
          model: "gpt-5.6-luna",
          monthlyBudgetUsd: 10,
        })}
      />,
    );

    await waitFor(() =>
      expect(screen.getByLabelText("Base URL")).toHaveValue("https://api3.gemai.cc"),
    );
    expect(screen.getByLabelText(/供应商名称/)).toHaveValue("我的中转站");
    expect(screen.getByLabelText("API Key")).toHaveValue("");
    expect(screen.getByRole("status")).toHaveTextContent("Windows 凭据管理器");
  });

  it("defaults to OpenAI Responses API and GPT-5.6 Luna", () => {
    render(<ModelSettings onSave={vi.fn()} />);

    expect(screen.getByLabelText("Base URL")).toHaveValue("https://api.openai.com/v1");
    expect(screen.getByLabelText("模型 ID")).toHaveValue("gpt-5.6-luna");
    expect(screen.getByLabelText("API Key")).toHaveAttribute("type", "password");
  });

  it("defaults to automatic protocol detection and allows an empty provider note", () => {
    render(<ModelSettings onSave={vi.fn()} />);

    expect(screen.getByLabelText("接口协议")).toHaveValue("auto");
    expect(screen.getByLabelText(/供应商名称/)).toHaveValue("");
  });

  it("offers a local privacy mode with an optional API key", () => {
    const onSave = vi.fn();
    render(<ModelSettings onSave={onSave} />);

    fireEvent.click(screen.getByLabelText("使用本地模型"));
    expect(screen.getByLabelText("Base URL")).toHaveValue("http://localhost:1234/v1");
    expect(screen.getByText(/API Key 可留空/)).toBeVisible();
  });

  it("restores the previous cloud configuration when local mode is disabled", () => {
    render(<ModelSettings onSave={vi.fn()} />);
    fireEvent.change(screen.getByLabelText(/供应商名称/), { target: { value: "我的中转站" } });
    fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "https://relay.example/v1" } });
    fireEvent.change(screen.getByLabelText("模型 ID"), { target: { value: "claude-sonnet-5" } });
    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: "sk-cloud-secret" } });

    const localToggle = screen.getByLabelText("使用本地模型");
    fireEvent.click(localToggle);
    fireEvent.click(localToggle);

    expect(screen.getByLabelText(/供应商名称/)).toHaveValue("我的中转站");
    expect(screen.getByLabelText("Base URL")).toHaveValue("https://relay.example/v1");
    expect(screen.getByLabelText("模型 ID")).toHaveValue("claude-sonnet-5");
    expect(screen.getByLabelText("API Key")).toHaveValue("sk-cloud-secret");
  });

  it("submits an OpenAI-compatible provider without exposing the key elsewhere", () => {
    const onSave = vi.fn();
    render(<ModelSettings onSave={onSave} />);

    fireEvent.change(screen.getByLabelText(/供应商名称/), { target: { value: "我的中转站" } });
    fireEvent.change(screen.getByLabelText("Base URL"), { target: { value: "https://relay.example/v1" } });
    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: "sk-test-secret" } });
    fireEvent.click(screen.getByRole("button", { name: "保存模型配置" }));

    expect(onSave).toHaveBeenCalledWith({
      providerName: "我的中转站",
      baseUrl: "https://relay.example/v1",
      protocol: "auto",
      model: "gpt-5.6-luna",
      apiKey: "sk-test-secret",
      monthlyBudgetUsd: 10,
    });
  });

  it("toggles API key visibility without moving or removing the control", () => {
    render(<ModelSettings onSave={vi.fn()} />);
    const keyInput = screen.getByLabelText("API Key");
    fireEvent.change(keyInput, { target: { value: "sk-test-secret" } });

    const showButton = screen.getByRole("button", { name: "显示 API Key" });
    fireEvent.click(showButton);
    expect(keyInput).toHaveAttribute("type", "text");
    expect(screen.getByRole("button", { name: "隐藏 API Key" })).toBe(showButton);

    fireEvent.click(showButton);
    expect(keyInput).toHaveAttribute("type", "password");
    expect(screen.getByRole("button", { name: "显示 API Key" })).toBe(showButton);
  });

  it("waits for persistence and shows a success confirmation", async () => {
    const onSave = vi.fn().mockResolvedValue(undefined);
    render(<ModelSettings onSave={onSave} />);
    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: "sk-test-secret" } });

    fireEvent.click(screen.getByRole("button", { name: "保存模型配置" }));

    await waitFor(() => expect(screen.getByText("配置已安全保存")).toBeVisible());
    expect(onSave).toHaveBeenCalledOnce();
  });

  it("shows the persistence error instead of silently failing", async () => {
    const onSave = vi.fn().mockRejectedValue(new Error("Windows 凭据管理器写入失败"));
    render(<ModelSettings onSave={onSave} />);
    fireEvent.change(screen.getByLabelText("API Key"), { target: { value: "sk-test-secret" } });

    fireEvent.click(screen.getByRole("button", { name: "保存模型配置" }));

    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("Windows 凭据管理器写入失败"),
    );
  });
});
