import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "jsdom",
    setupFiles: ["./tests/setup.ts"],
    // 本机负载高时首测常吃满 5s（环境初始化），放宽到 20s。
    testTimeout: 20_000,
  },
});
