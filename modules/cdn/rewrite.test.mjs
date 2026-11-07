// Unit checks for the CloudFront viewer-request rewrite.
//
// The production file (`rewrite.js`) must stay plain `cloudfront-js-2.0`:
// no imports, no exports, no Node APIs. The test loads its source and evaluates
// it in a fresh VM context, then exercises the exported `handler`.
//
//   node --test modules/cdn/rewrite.test.mjs

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("./rewrite.js", import.meta.url), "utf8");
const handler = vm.runInNewContext(`${source}\nhandler;`, {});

function rewrite(uri, querystring = "") {
  return handler({ request: { uri, querystring } });
}

test("root maps to index.html", () => {
  assert.equal(rewrite("/").uri, "/index.html");
});

test("slash-terminated path maps to index.html", () => {
  assert.equal(rewrite("/timeline/").uri, "/timeline/index.html");
});

test("extensionless path appends .html", () => {
  assert.equal(
    rewrite("/timeline/ohio-senate-2026").uri,
    "/timeline/ohio-senate-2026.html",
  );
});

test("nested extensionless path appends .html to the last segment", () => {
  assert.equal(rewrite("/a/b/c").uri, "/a/b/c.html");
});

test("a dot in an earlier segment does not suppress the append", () => {
  assert.equal(rewrite("/v1.2/page").uri, "/v1.2/page.html");
});

test("existing extension is left alone", () => {
  assert.equal(rewrite("/timeline/x.html").uri, "/timeline/x.html");
  assert.equal(rewrite("/media/cover/x.jpg").uri, "/media/cover/x.jpg");
});

test("query string is untouched", () => {
  const request = rewrite("/timeline/x", "a=1&b=2");
  assert.equal(request.uri, "/timeline/x.html");
  assert.equal(request.querystring, "a=1&b=2");
});
