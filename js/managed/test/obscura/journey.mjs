async function journey(call, events, checks) {
  const eq = (name, a, b) => {
    const pass = JSON.stringify(a) === JSON.stringify(b);
    checks.push({ name, actual: a, expected: b, pass });
    if (!pass)
      throw new Error(
        name + ": " + JSON.stringify(a) + " != " + JSON.stringify(b),
      );
  };
  const { targetId } = await call("Target.createTarget", {
    url: "about:blank",
  });
  const { sessionId } = await call("Target.attachToTarget", {
    targetId,
    flatten: true,
  });
  const page = (m, p = {}) => call(m, p, sessionId);
  await page("Page.enable");
  await page("Runtime.enable");
  await page("Page.navigate", { url: "https://fixture.example.com/parent" });
  const evalAt = (expression, contextId) =>
    page("Runtime.evaluate", { expression, contextId, returnByValue: true });
  let tree;
  for (let i = 0; i < 50; i++) {
    tree = await page("Page.getFrameTree");
    if (tree.frameTree.childFrames.length) break;
    await new Promise((r) => setTimeout(r, 10));
  }
  eq("iframe has actual frame", tree.frameTree.childFrames.length, 1);
  const childId = tree.frameTree.childFrames[0].frame.id;
  const child = events
    .filter(
      (e) =>
        e.method === "Runtime.executionContextCreated" &&
        e.params.context.auxData.frameId === childId,
    )
    .at(-1)?.params.context.id;
  eq("child default context announced", typeof child, "number");
  eq(
    "script executes and load event fires",
    (await evalAt("scriptResult")).result.value,
    { executed: 1, loaded: 1 },
  );
  eq(
    "child default evaluation selects child",
    (await evalAt("document.title", child)).result.value,
    "Child",
  );
  eq(
    "parent remains parent",
    (await evalAt("document.title")).result.value,
    "Parent",
  );
  const called = await page("Runtime.callFunctionOn", {
    executionContextId: child,
    functionDeclaration:
      'function(value){document.querySelector("input").value=value;return document.title}',
    arguments: [{ value: "child-edited" }],
    returnByValue: true,
  });
  eq("callFunctionOn uses child", called.result.value, "Child");
  eq(
    "parent input unchanged",
    (await evalAt('document.querySelector("input").value')).result.value,
    "parent",
  );
  const doc = await page("DOM.getDocument", { depth: -1, pierce: true });
  const flat = [];
  function walk(n) {
    flat.push(n);
    n.children?.forEach(walk);
    if (n.contentDocument) walk(n.contentDocument);
  }
  walk(doc.root);
  const input = flat.find(
    (n) => n.nodeName === "INPUT" && n.attributes.includes("child-input"),
  );
  eq("DOM pierces real child", !!input, true);
  const childDoc = flat.find(
    (n) => n.documentURL === "https://child.example.com/frame",
  );
  const q = await page("DOM.querySelector", {
    nodeId: childDoc.nodeId,
    selector: "#child-input",
  });
  eq("child scoped DOM query", q.nodeId, input.nodeId);
  await page("DOM.focus", { nodeId: input.nodeId });
  await page("Input.insertText", { text: "-typed" });
  eq(
    "typing reaches child",
    (await evalAt('document.querySelector("input").value', child)).result.value,
    "child-edited-typed",
  );
  const resolved = await page("DOM.resolveNode", { nodeId: input.nodeId });
  const value = await page("Runtime.callFunctionOn", {
    objectId: resolved.object.objectId,
    functionDeclaration: "function(){return this.value}",
    returnByValue: true,
  });
  eq(
    "resolved child node remote object",
    value.result.value,
    "child-edited-typed",
  );
  await page("Runtime.releaseObject", { objectId: resolved.object.objectId });
  const promise = await page("Runtime.evaluate", {
    expression: 'new Promise(r=>setTimeout(()=>r("resolved"),10))',
    awaitPromise: true,
    returnByValue: true,
  });
  eq("promise runs jobs and timers", promise.result.value, "resolved");
  const err = await page("Runtime.evaluate", {
    expression: 'throw new Error("fixture error")',
    returnByValue: true,
  });
  eq("exception reported", !!err.exceptionDetails, true);
  let rejected = false;
  try {
    await page("Page.captureScreenshot");
  } catch (e) {
    rejected = /does not implement/.test(String(e));
  }
  eq("unsupported renderer fails explicitly", rejected, true);
  await page("Page.navigate", { url: "about:blank" });
  rejected = false;
  try {
    await evalAt("document.title", child);
  } catch {
    rejected = true;
  }
  eq("stale child context rejected", rejected, true);
  await call("Target.closeTarget", { targetId });
  eq(
    "target close cleans up",
    (await call("Target.getTargets")).targetInfos.length,
    0,
  );
  return checks;
}
const fixtures = {
  "https://fixture.example.com/parent":
    '<!doctype html><title>Parent</title><input id="parent-input" value="parent"><iframe src="https://child.example.com/frame"></iframe><script>globalThis.scriptResult={executed:0,loaded:0};const s=document.createElement("script");s.src="/external.js";s.async=true;s.addEventListener("load",()=>scriptResult.loaded++);document.head.appendChild(s)<\/script>',
  "https://fixture.example.com/external.js": "scriptResult.executed++",
  "https://child.example.com/frame":
    '<!doctype html><title>Child</title><input id="child-input" value="child">',
};
export { fixtures, journey };
