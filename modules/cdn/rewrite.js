// CloudFront Function (runtime cloudfront-js-2.0) — viewer-request URI rewrite.
//
// The renderer writes real `.html` objects (`static_file_path`), but readers
// use extensionless URLs (`/timeline/<id>`). This function maps:
//
//   /                 -> /index.html
//   /timeline/        -> /timeline/index.html
//   /timeline/<id>    -> /timeline/<id>.html
//   /timeline/<id>.html (or any last segment with a dot) -> unchanged
//
// Only the final path segment is inspected, so a dot in an earlier segment
// (`/v1.2/page`) still gets `.html`. The query string is preserved by
// CloudFront and is never touched here.
//
// Keep this file plain `cloudfront-js-2.0`: no imports, exports, or Node APIs.
// `rewrite.test.mjs` evaluates it in a VM and checks the rules.

function handler(event) {
  var request = event.request;
  var uri = request.uri;

  if (uri.endsWith("/")) {
    request.uri = uri + "index.html";
    return request;
  }

  var lastSegment = uri.substring(uri.lastIndexOf("/") + 1);
  if (lastSegment.indexOf(".") === -1) {
    request.uri = uri + ".html";
  }

  return request;
}
