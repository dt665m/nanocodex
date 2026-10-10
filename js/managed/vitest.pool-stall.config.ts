// Runs only the synthetic #951 stall fixture, without the settle hook, for
// test/workers-pool-diagnostics-journey.test.mjs. Not part of "vitest run".
import { defineConfig, type UserConfig } from "vitest/config";
import base from "./vitest.config.ts";

export default defineConfig(async env => {
  const config = await (base as (env: unknown) => Promise<UserConfig>)(env);
  return { ...config, test: { ...config.test, include: ["test-fixtures/workers-pool-stall/*.test.ts"], setupFiles: [] } };
});
