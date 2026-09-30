import { fixtures } from "./journey.mjs";
const storageFixtures = {
  "https://fixture.example.com/storage-a":
    "<!doctype html><title>Storage A</title>",
  "https://fixture.example.com/storage-b":
    "<!doctype html><title>Storage B</title>",
  "https://child.example.com/storage":
    "<!doctype html><title>Other Origin</title>",
};
export default {
  async fetch(request) {
    if (request.url === "https://fixture.example.com/large.js") {
      return new Response(
        "/*" +
          "x".repeat(3 * 1024 * 1024) +
          "*/globalThis.largeScriptLoaded=true",
        { headers: { "content-type": "text/javascript" } },
      );
    }
    if (request.url === "https://fixture.example.com/no-content")
      return new Response(null, { status: 204 });
    if (request.url === "https://fixture.example.com/oversized") {
      let remaining = 3 * 1024 * 1024;
      return new Response(
        new ReadableStream({
          pull(controller) {
            if (!remaining) return controller.close();
            const size = Math.min(remaining, 64 * 1024);
            remaining -= size;
            controller.enqueue(new Uint8Array(size));
          },
        }),
      );
    }
    const body = fixtures[request.url] ?? storageFixtures[request.url];
    return new Response(body ?? "Not found", {
      status: body === void 0 ? 404 : 200,
      headers: {
        "content-type": request.url.endsWith(".js")
          ? "text/javascript"
          : "text/html",
      },
    });
  },
};
