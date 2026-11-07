#!/usr/bin/env python3
"""Drive the local OIDC authorization-code + PKCE flow with the stdlib.

This is the scripted equivalent of opening http://localhost:8081/api/auth/login
in a browser: it follows the redirect to Keycloak, posts the demo credentials,
hits /api/auth/callback, checks /api/auth/me with the cookie, then logs out.

Python's cookie jar refuses to send a `Secure` cookie over plain HTTP, while
browsers treat localhost as a secure context. A localhost-specific cookie
policy closes that gap; the session id is also forwarded explicitly for the
`/api/auth/me` check.

Usage:
    python scripts/keycloak-login.py [--base http://localhost:8081] \
        [--username demo] [--password demo]
"""

import argparse
import html.parser
import http.cookiejar
import re
import sys
import urllib.error
import urllib.parse
import urllib.request

REDIRECT_CODES = {301, 302, 303, 307, 308}


def status_of(response):
    return getattr(response, "status", None) or response.code


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


class LocalhostCookiePolicy(http.cookiejar.DefaultCookiePolicy):
    """Send `Secure` cookies to localhost over HTTP, as browsers do."""

    def return_ok_secure(self, cookie, request):
        return True


class LoginFormParser(html.parser.HTMLParser):
    def __init__(self, form_id="kc-form-login"):
        super().__init__()
        self.form_id = form_id
        self.action = None
        self.inputs = {}

    def handle_starttag(self, tag, attrs):
        attributes = dict(attrs)
        if tag == "form" and attributes.get("id") == self.form_id:
            self.action = attributes.get("action")
        if tag == "input" and attributes.get("name"):
            self.inputs[attributes["name"]] = attributes.get("value", "")


def request(opener, url, data=None, method=None, headers=None):
    try:
        if method or headers:
            request = urllib.request.Request(
                url, data=data, method=method, headers=headers or {}
            )
            return opener.open(request)
        return opener.open(url, data)
    except urllib.error.HTTPError as error:
        # NoRedirect makes urllib raise the 3xx; it is still a usable response.
        return error


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", default="http://localhost:8081")
    parser.add_argument("--username", default="demo")
    parser.add_argument("--password", default="demo")
    args = parser.parse_args()
    base = args.base.rstrip("/")

    jar = http.cookiejar.CookieJar(LocalhostCookiePolicy())
    opener = urllib.request.build_opener(
        urllib.request.HTTPCookieProcessor(jar), NoRedirect()
    )

    login = request(opener, f"{base}/api/auth/login")
    if status_of(login) not in REDIRECT_CODES:
        print(f"login did not redirect: {status_of(login)}", file=sys.stderr)
        return 1
    authorize_url = login.headers["Location"]
    print(f"1. /api/auth/login -> {authorize_url}")

    response = request(opener, authorize_url)
    current = authorize_url
    while status_of(response) in REDIRECT_CODES:
        current = urllib.parse.urljoin(current, response.headers["Location"])
        response = request(opener, current)
    page = response.read().decode()
    print(f"2. Keycloak login page at {current} ({status_of(response)})")

    form = LoginFormParser()
    form.feed(page)
    if not form.action:
        print("no kc-form-login action in the login page", file=sys.stderr)
        return 1
    fields = dict(form.inputs)
    fields["username"] = args.username
    fields["password"] = args.password
    action = urllib.parse.urljoin(current, form.action.replace("&amp;", "&"))
    print(f"3. POST credentials ({args.username}) to {action}")

    response = request(opener, action, urllib.parse.urlencode(fields).encode())
    reached_callback = False
    while status_of(response) in REDIRECT_CODES:
        next_url = urllib.parse.urljoin(action, response.headers["Location"])
        if "/api/auth/callback" in next_url:
            response = request(opener, next_url)
            reached_callback = True
            break
        response = request(opener, next_url)

    if not reached_callback and "/api/auth/callback" not in (
        response.headers.get("Location") or ""
    ):
        print(
            f"callback was not reached (status {status_of(response)})",
            file=sys.stderr,
        )
        print(response.read().decode()[:2000], file=sys.stderr)
        return 1

    set_cookies = response.headers.get_all("Set-Cookie") or []
    set_cookie = next(
        (value for value in set_cookies if value.startswith("timeline_session=")), ""
    )
    print(
        f"4. /api/auth/callback -> {status_of(response)} "
        f"{response.headers.get('Location')}"
    )
    for value in set_cookies:
        print(f"   Set-Cookie: {value}")
    match = re.match(r"timeline_session=([^;]+)", set_cookie)
    if not match:
        print("callback did not set timeline_session", file=sys.stderr)
        return 1
    session = match.group(1)
    headers = {"Cookie": f"timeline_session={session}"}

    me = request(opener, f"{base}/api/auth/me", headers=headers)
    print(f"5. /api/auth/me -> {me.read().decode()}")

    logout = request(
        opener,
        f"{base}/api/auth/logout",
        data=b"",
        method="POST",
        headers=headers,
    )
    print(
        f"6. POST /api/auth/logout -> {status_of(logout)} "
        f"{logout.headers.get('Set-Cookie')}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
