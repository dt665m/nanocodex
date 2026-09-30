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
