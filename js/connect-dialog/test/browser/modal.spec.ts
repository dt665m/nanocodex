import { expect, test, type Page, type TestInfo } from "@playwright/test";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";

const evidence = resolve(process.cwd(), "../../output/connect-full-page");
const sizes = [
  { name: "desktop", width: 1280, height: 900 },
  { name: "mobile", width: 390, height: 844 },
  { name: "short", width: 390, height: 440 },
];

async function contained(page: Page) {
  const geometry = await page.evaluate(() => {
    const shell = document.querySelector(".dialog-shell")!;
    const box = shell.getBoundingClientRect();
    return {
      width: innerWidth, height: innerHeight,
      documentWidth: document.documentElement.scrollWidth,
      documentHeight: document.documentElement.scrollHeight,
      left: box.left, top: box.top, right: box.right, bottom: box.bottom,
      horizontalOverflow: [...shell.querySelectorAll("*")].filter(el => {
        const style = getComputedStyle(el);
        return style.overflowX === "visible" && el.scrollWidth > el.clientWidth + 1 && el.clientWidth > 0;
      }).map(el => `${el.tagName}.${el.className}`),
    };
  });
  expect(geometry.documentWidth).toBeLessThanOrEqual(geometry.width);
  expect(geometry.documentHeight).toBeLessThanOrEqual(geometry.height);
  expect(geometry.left).toBe(0);
  expect(geometry.top).toBe(0);
  expect(geometry.right).toBe(geometry.width);
  expect(geometry.bottom).toBe(geometry.height);
  expect(geometry.horizontalOverflow).toEqual([]);
  return geometry;
}

async function screenshot(page: Page, info: TestInfo, name: string) {
  const path = resolve(evidence, `${info.title.replace(/\W+/g, "-")}-${name}.png`);
  await page.screenshot({ path });
  await info.attach(name, { path, contentType: "image/png" });
}

for (const theme of ["light", "dark"] as const) {
  for (const size of sizes) {
    test(`${theme} ${size.name} SMS recovery and consent`, async ({ page }, info) => {
      await mkdir(evidence, { recursive: true });
      await page.setViewportSize(size);
      await page.emulateMedia({ colorScheme: theme, reducedMotion: "reduce" });
      const errors: string[] = [];
      const requests: { path: string; body: unknown; status?: number }[] = [];
      page.on("pageerror", error => errors.push(error.message));
      page.on("response", response => {
        const request = response.request();
        if (new URL(request.url()).pathname.startsWith("/v1/")) requests.push({
          path: new URL(request.url()).pathname,
          body: request.postDataJSON(), status: response.status(),
        });
      });
      await page.route("**/*", route => new URL(route.request().url()).hostname === "modal.nanocodex.localhost"
        ? route.continue() : route.abort("blockedbyclient"));
      await page.goto("/");
      const phone = page.getByRole("textbox", { name: "Mobile number" });
      await expect(phone).toBeFocused();
      await contained(page);
      await screenshot(page, info, "phone");
      await phone.fill("123456");
      await phone.press("Enter");
      await expect(page.getByRole("alert")).toContainText("verification code");
      expect(requests.filter(r => r.path.endsWith("/sms/start"))).toHaveLength(0);
      await expect(phone).toHaveAttribute("aria-invalid", "true");
      await phone.fill("+1 202 555 0000");
      await phone.press("Enter");
      await expect(page.getByRole("alert")).toContainText("could not be delivered");
      await phone.fill("+1 202 555 0100");
      await phone.press("Tab");
      const send = page.getByRole("button", { name: "Text me a code" });
      await expect(send).toBeFocused();
      expect(await send.evaluate(el => getComputedStyle(el).outlineStyle)).not.toBe("none");
      await page.keyboard.press("Enter");
      const code = page.getByRole("textbox", { name: "6-digit code" });
      await expect(code).toBeVisible();
      await expect(code).toBeFocused();
      await contained(page);
      await screenshot(page, info, "code");
      await code.fill("12");
      await code.press("Enter");
      await expect(page.getByRole("alert")).toContainText("six-digit");
      expect(requests.filter(r => r.path.endsWith("/sms/verify"))).toHaveLength(0);
      await code.fill("111111");
      await code.press("Enter");
      await expect(page.getByRole("alert")).toContainText("invalid or expired");
      await contained(page);
      await screenshot(page, info, "invalid-code");
      await page.getByRole("button", { name: "Use a different number" }).click();
      await expect(phone).toBeVisible();
      await expect(phone).toBeFocused();
      await expect(page.getByRole("alert")).toHaveCount(0);
      await phone.fill("+1 202 555 0101");
      await phone.press("Enter");
      await code.fill("123456");
      await code.press("Enter");
      const approve = page.getByRole("button", { name: "Allow access" });
      await expect(approve).toBeEnabled();
      await expect(page.getByRole("heading", { name: "Connect to Atlas Workspace" })).toBeFocused();
      expect(await page.locator(".dialog-content").evaluate(el => el.scrollTop)).toBe(0);
      expect(await page.evaluate(() => (window as any).__hostReceipt)).toBeUndefined();
      const geometry = await contained(page);
      await screenshot(page, info, "consent");
      await approve.focus();
      await page.keyboard.press("Enter");
      await expect(page.getByRole("status")).toHaveText("Request approved");
      const receipt = await page.evaluate(() => (window as any).__hostReceipt);
      expect(receipt.kind).toBe("approved");
      expect(receipt.result.accounts[0].capabilities.auth.mode).toBe("hosted");
      expect(requests.filter(r => r.path.endsWith("/sms/start")).map(r => r.body)).toEqual([
        { phone: "+12025550000" }, { phone: "+12025550100" }, { phone: "+12025550101" },
      ]);
      expect(errors).toEqual([]);
      await writeFile(resolve(evidence, `${theme}-${size.name}-receipt.json`), JSON.stringify({ theme, viewport: size, geometry, requests, receipt, errors }, null, 2));
    });
  }
}

test("cancel and Escape never approve access", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Request cancelled");
  expect(await page.evaluate(() => (window as any).__hostReceipt.kind)).toBe("cancelled");
  await page.reload();
  await expect(page.getByRole("textbox", { name: "Mobile number" })).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("status")).toHaveText("Request cancelled");
  await page.reload();
  await page.getByRole("textbox", { name: "Mobile number" }).fill("+12025550100");
  await page.getByRole("button", { name: "Text me a code" }).click();
  await page.getByRole("textbox", { name: "6-digit code" }).fill("123456");
  await page.getByRole("button", { name: "Continue", exact: true }).click();
  await expect(page.getByRole("button", { name: "Allow access" })).toBeEnabled();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Request cancelled");
  expect(await page.evaluate(() => (window as any).__hostReceipt.kind)).toBe("cancelled");
});

for (const variant of [
  { theme: "light" as const, width: 1280, height: 900 },
  { theme: "dark" as const, width: 1280, height: 900 },
  { theme: "light" as const, width: 390, height: 844 },
  { theme: "dark" as const, width: 390, height: 844 },
]) {
  test(`${variant.theme} ${variant.width > 620 ? "desktop" : "mobile"} requested connection groups`, async ({ page }, info) => {
    await page.setViewportSize(variant);
    await page.emulateMedia({ colorScheme: variant.theme });
    await page.goto("/?connections=1");
    await page.getByRole("textbox", { name: "Mobile number" }).fill("+12025550100");
    await page.getByRole("button", { name: "Text me a code" }).click();
    await page.getByRole("textbox", { name: "6-digit code" }).fill("123456");
    await page.getByRole("button", { name: "Continue", exact: true }).click();
    await expect(page.getByRole("heading", { name: "Connect to Atlas Workspace" })).toBeFocused();
    await expect(page.getByRole("button", { name: /GitHub.*Connected/ })).toBeDisabled();
    const connection = page.getByRole("button", { name: /Google Workspace.*Connect/ });
    await expect(connection).toBeEnabled();
    await expect(page.getByRole("button", { name: "Allow access", exact: true })).toBeDisabled();
    await contained(page);
    await screenshot(page, info, "connections");
    await page.keyboard.press("Tab");
    await expect(connection).toBeFocused();
    expect(await connection.evaluate(el => getComputedStyle(el).outlineOffset)).toBe("-3px");
    expect(await page.evaluate(() => (window as any).__hostReceipt)).toBeUndefined();
    await page.getByRole("button", { name: "Cancel", exact: true }).click();
    await expect(page.getByRole("status")).toHaveText("Request cancelled");
  });
}

test("SDK popup presents the desktop authorization layout", async ({ page }) => {
  await page.goto("/launcher.html");
  const opened = page.waitForEvent("popup");
  await page.getByRole("button", { name: "Connect account" }).click();
  const popup = await opened;
  await popup.getByRole("textbox", { name: "Mobile number" }).fill("+12025550100");
  await popup.getByRole("button", { name: "Text me a code" }).click();
  await popup.getByRole("textbox", { name: "6-digit code" }).fill("123456");
  await popup.getByRole("button", { name: "Continue", exact: true }).click();
  await expect(popup.getByRole("heading", { name: "Connect to Atlas Workspace" })).toBeFocused();
  const intro = await popup.locator(".wizard-review-page > .wizard-intro").boundingBox();
  const access = await popup.locator(".wizard-review-page > .wizard-sections").boundingBox();
  expect(access!.x).toBeGreaterThan(intro!.x + intro!.width);
  await contained(popup);
  await popup.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(popup.getByRole("status")).toHaveText("Request cancelled");
  await popup.close();
});
