/* Timelines read-page behaviour (ticket 04: vote controls tell the truth).
 *
 * Loaded after htmx on every Timeline read page, as a real asset
 * (/assets/app.js), never an inline blob. The file has four sections:
 *
 *   1. Vote messages      - the mapping from a refusal status to the sentence a
 *                           reader sees. `crates/renderer/src/ssg.rs` asserts
 *                           these literals, so changing one changes the test.
 *   2. Vote refusals      - htmx:responseError / htmx:sendError become a visible
 *                           status in the widget. 401 offers a sign-in link that
 *                           comes back to this Timeline; the counts stay put.
 *   3. Own-vote paint     - one GET /api/timeline/{id}/votes, then every widget
 *                           on the page is marked with the reader's own vote.
 *   4. Extension points   - ticket 05 (header/auth) and ticket 13 (live-region
 *                           announcements) append here; see the markers below.
 *
 * Every message is written as text nodes, never as HTML: a refusal body is
 * never swapped into the page.
 */
(function () {
  "use strict";

  // ---- 1. Vote messages -----------------------------------------------------

  // One entry per status the vote endpoints can return to a browser. Keep the
  // `status: "text"` shape: the Rust test greps these exact lines.
  var VOTE_MESSAGES = {
    401: "Sign in to vote",
    403: "You cannot vote on your own timeline.",
    409: "Your vote changed in another tab. Reload the page and try again.",
    429: "You are voting too quickly."
  };

  // Any other refusal: still visible, never a silent no-op.
  var VOTE_MESSAGE_FALLBACK = "Voting is unavailable right now. Your vote was not saved.";
  // No response at all (offline, DNS, aborted request).
  var VOTE_MESSAGE_NETWORK = "Could not reach the server. Check your connection and try again.";
  // 429 with and without a usable Retry-After header.
  var RETRY_SUFFIX = " Try again in {seconds} seconds.";
  var RETRY_SUFFIX_UNKNOWN = " Try again in a moment.";

  // Own-vote state values, matching data-my-vote and the fragment.
  var MY_VOTE_NONE = "none";
  var MY_VOTE_UP = "up";
  var MY_VOTE_DOWN = "down";

  var VOTE_SELECTOR = ".vote[data-vote-target]";

  // ---- 2. Vote refusals -----------------------------------------------------

  // The reader returns to the same Timeline (path and anchor) after signing in.
  function signInHref() {
    var next = window.location.pathname + window.location.hash;
    return "/api/auth/login?next=" + encodeURIComponent(next);
  }

  // The widget that owns the element htmx reported, if any.
  function owningWidget(element) {
    if (!element || !element.closest) {
      return null;
    }
    return element.closest(".vote");
  }

  // `Retry-After` is whole seconds on the vote endpoints; anything else is
  // ignored rather than guessed at.
  function retryAfterSeconds(xhr) {
    if (!xhr || !xhr.getResponseHeader) {
      return null;
    }
    var raw = xhr.getResponseHeader("Retry-After");
    if (!raw) {
      return null;
    }
    var seconds = parseInt(raw, 10);
    if (isNaN(seconds) || seconds <= 0) {
      return null;
    }
    return seconds;
  }

  // Replace the status text. Text nodes and elements only: never innerHTML.
  function setStatus(status, content) {
    if (!status) {
      return;
    }
    while (status.firstChild) {
      status.removeChild(status.firstChild);
    }
    if (content) {
      status.appendChild(content);
      status.hidden = false;
    } else {
      status.hidden = true;
    }
  }

  // The visible explanation for one refusal.
  function refusalContent(status, xhr) {
    if (status === 401) {
      var link = document.createElement("a");
      link.href = signInHref();
      link.textContent = VOTE_MESSAGES[401];
      return link;
    }
    var text = VOTE_MESSAGES[status] || VOTE_MESSAGE_FALLBACK;
    if (status === 429) {
      var seconds = retryAfterSeconds(xhr);
      text += seconds === null ? RETRY_SUFFIX_UNKNOWN : RETRY_SUFFIX.replace("{seconds}", String(seconds));
    }
    return document.createTextNode(text);
  }

  function showRefusal(event) {
    var detail = event.detail || {};
    var widget = owningWidget(detail.elt || detail.target);
    if (!widget) {
      // Not a vote request: leave the event to whoever else handles it.
      return;
    }
    var xhr = detail.xhr;
    var status = xhr ? xhr.status : 0;
    setStatus(widget.querySelector(".vote__status"), refusalContent(status, xhr));
  }

  function showNetworkFailure(event) {
    var detail = event.detail || {};
    var widget = owningWidget(detail.elt || detail.target);
    if (!widget) {
      return;
    }
    setStatus(widget.querySelector(".vote__status"), document.createTextNode(VOTE_MESSAGE_NETWORK));
  }

  // ---- 3. Own-vote paint ----------------------------------------------------

  // `/timeline/<id>` is the only page with widgets; anything else paints nothing.
  function timelineIdFromPath(pathname) {
    var match = /^\/timeline\/([^\/]+)\/?$/.exec(pathname);
    return match ? decodeURIComponent(match[1]) : null;
  }

  function paint(widget, direction) {
    var value = direction === MY_VOTE_UP || direction === MY_VOTE_DOWN ? direction : MY_VOTE_NONE;
    widget.setAttribute("data-my-vote", value);

    // Only the two direction buttons are toggles: the clear action carries no
    // data-vote-direction, so it never gets a meaningless aria-pressed.
    var buttons = widget.querySelectorAll(".vote__btn[data-vote-direction]");
    for (var index = 0; index < buttons.length; index += 1) {
      var button = buttons[index];
      var pressed = button.getAttribute("data-vote-direction") === value;
      button.setAttribute("aria-pressed", pressed ? "true" : "false");
    }

    var clear = widget.querySelector(".vote__btn--clear");
    if (clear) {
      clear.hidden = value === MY_VOTE_NONE;
    }
  }

  function paintAll(timelineId, own) {
    var events = own.events || {};
    var widgets = document.querySelectorAll(VOTE_SELECTOR);
    for (var index = 0; index < widgets.length; index += 1) {
      var widget = widgets[index];
      var target = widget.getAttribute("data-vote-target");
      var direction = target === timelineId ? own.timeline : events[target];
      paint(widget, direction);
    }
  }

  // One request per Timeline page. Anonymous readers get 401; that is not an
  // error condition, so the neutral widget simply stays unpainted.
  function paintOwnVotes() {
    var timelineId = timelineIdFromPath(window.location.pathname);
    if (!timelineId) {
      return;
    }
    var url = "/api/timeline/" + encodeURIComponent(timelineId) + "/votes";
    window
      .fetch(url, {
        credentials: "same-origin",
        headers: { Accept: "application/json" }
      })
      .then(function (response) {
        return response.ok ? response.json() : null;
      })
      .then(function (own) {
        if (own) {
          paintAll(timelineId, own);
        }
      })
      .catch(function () {
        // Writes still work; the error path explains a refusal. Nothing to say
        // about a failed read of optional decoration.
      });
  }

  // ---- 4. Extension points --------------------------------------------------

  // Header/auth (ticket 05): append a section here that paints the account area
  // from GET /api/auth/me and renders the POST sign-out form.
  //
  // Live regions (ticket 13): append announcement helpers here and reuse
  // `setStatus` / the widget's `.vote__status` element instead of adding a
  // second status region per widget.

  function init() {
    document.body.addEventListener("htmx:responseError", showRefusal);
    document.body.addEventListener("htmx:sendError", showNetworkFailure);
    paintOwnVotes();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
