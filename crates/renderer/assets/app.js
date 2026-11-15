/* Timelines site script.
 *
 * Loaded deferred on every page (static read pages and the API's form/refusal
 * pages alike), as a real asset (/assets/app.js), never an inline blob. Pages
 * are generated without knowing the viewer, so every per-viewer affordance ships
 * as anonymous markup and is upgraded here. Nothing here is a source of truth:
 * the API enforces every rule.
 *
 * Sections:
 *
 *   1. Account chrome     - GET /api/auth/me upgrades the header (name, Sign out,
 *                           New timeline) and reveals `[data-owner-only]` after
 *                           GET /api/timeline/{id}/access says the reader owns
 *                           the Timeline (ticket 05).
 *   2. Vote messages      - the mapping from a refusal status to the sentence a
 *                           reader sees. `crates/renderer/src/ssg.rs` asserts
 *                           these literals, so changing one changes the test.
 *   3. Vote refusals      - htmx:responseError / htmx:sendError become a visible
 *                           status in the widget. 401 offers a sign-in link that
 *                           comes back to this Timeline; the counts stay put.
 *   4. Own-vote paint     - one GET /api/timeline/{id}/votes, then every widget
 *                           on the page is marked with the reader's own vote.
 *   5. Extension points   - ticket 13 (live-region announcements) appends here;
 *                           see the markers below.
 *
 * Every message is written as text nodes, never as HTML: a refusal body is
 * never swapped into the page.
 */

/* ---- 1. Account chrome (ticket 05) ---------------------------------------- */

(function () {
  "use strict";

  var account = document.querySelector("[data-account]");
  if (!account) return;

  var signIn = account.querySelector("[data-sign-in]");
  var name = account.querySelector("[data-account-name]");
  var newTimeline = account.querySelector("[data-new-timeline]");
  var signOut = account.querySelector("[data-sign-out]");

  // The static href returns Home; send the reader back to the page they are on.
  if (signIn) {
    var here = window.location.pathname + window.location.search;
    signIn.setAttribute("href", "/api/auth/login?next=" + encodeURIComponent(here));
  }

  // Owner-only controls: static HTML carries `hidden` plus `data-owner-only` and
  // `data-timeline-id`; only the API's access check may reveal them. Anonymous
  // readers never see owner tools.
  function ownedControls(timelineId) {
    var candidates = document.querySelectorAll("[data-owner-only]");
    var matched = [];
    for (var i = 0; i < candidates.length; i += 1) {
      if (candidates[i].getAttribute("data-timeline-id") === timelineId) {
        matched.push(candidates[i]);
      }
    }
    return matched;
  }

  function revealOwned(timelineIds) {
    timelineIds.forEach(function (timelineId) {
      window
        .fetch("/api/timeline/" + encodeURIComponent(timelineId) + "/access", {
          credentials: "same-origin",
          headers: { Accept: "application/json" }
        })
        .then(function (response) {
          return response.ok ? response.json() : null;
        })
        .then(function (access) {
          if (!access || access.canEdit !== true) return;
          ownedControls(timelineId).forEach(function (control) {
            control.removeAttribute("hidden");
          });
        })
        .catch(function () {
          // The controls stay hidden; the pages stay readable.
        });
    });
  }

  function timelineIdsWithOwnerControls() {
    var candidates = document.querySelectorAll("[data-owner-only][data-timeline-id]");
    var ids = [];
    for (var i = 0; i < candidates.length; i += 1) {
      var id = candidates[i].getAttribute("data-timeline-id");
      if (id && ids.indexOf(id) === -1) ids.push(id);
    }
    return ids;
  }

  function showSignedIn(displayName) {
    if (signIn) signIn.setAttribute("hidden", "");
    if (name) {
      // textContent, never innerHTML: a display name is user-controlled text.
      name.textContent = displayName;
      name.removeAttribute("hidden");
    }
    if (newTimeline) newTimeline.removeAttribute("hidden");
    if (signOut) signOut.removeAttribute("hidden");
  }

  window
    .fetch("/api/auth/me", {
      credentials: "same-origin",
      headers: { Accept: "application/json" }
    })
    .then(function (response) {
      return response.ok ? response.json() : null;
    })
    .then(function (me) {
      if (!me || me.authenticated !== true || !me.account) return;
      showSignedIn(me.account.displayName || "Signed in");
      revealOwned(timelineIdsWithOwnerControls());
    })
    .catch(function () {
      // Anonymous chrome remains: "Sign in", and no owner controls.
    });
})();

/* ---- 2-5. Vote controls (ticket 04) -------------------------------------- */

(function () {
  "use strict";

  // ---- 2. Vote messages -----------------------------------------------------

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

  // ---- 3. Vote refusals -----------------------------------------------------

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

  // ---- 4. Own-vote paint ----------------------------------------------------

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

  // ---- 5. Extension points --------------------------------------------------

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

/* ---- 6. Pending suggestions (ticket 09) ---------------------------------- */

(function () {
  "use strict";

  var section = document.querySelector("[data-pending-suggestions]");
  if (!section) return;

  var timelineId = section.getAttribute("data-timeline-id");
  if (!timelineId) return;

  var list = section.querySelector("[data-pending-list]");
  var empty = section.querySelector("[data-pending-empty]");
  var queueLink = document.querySelector("[data-owner-only][href=\"/api/suggestions/queue\"]");

  function kindLabel(kind) {
    return kind === "APPEND" ? "Proposed new event" : "Proposed edit";
  }

  function headline(suggestion) {
    var payload = suggestion.payload || {};
    if (suggestion.kind === "EDIT") {
      var fields = ["date", "title", "description", "sources", "significance"].filter(function (field) {
        return Object.prototype.hasOwnProperty.call(payload, field);
      });
      return fields.length ? "Edit to the " + fields.join(", ") : "Edit";
    }
    return payload.title || "New event";
  }

  function entry(suggestion) {
    var item = document.createElement("li");
    item.className = "pending__item";

    var title = document.createElement("p");
    title.className = "pending__item-title";
    // textContent: a proposal is user-controlled text, never markup.
    title.textContent =
      kindLabel(suggestion.kind) +
      (suggestion.kind === "APPEND" ? ": " + headline(suggestion) : "");

    var meta = document.createElement("p");
    meta.className = "pending__item-meta";
    var parts = [];
    if (suggestion.submittedByName) parts.push("By " + suggestion.submittedByName);
    if (suggestion.kind === "EDIT") parts.push(headline(suggestion));
    parts.push("Awaiting review");
    meta.textContent = parts.join(" \u00b7 ");

    item.appendChild(title);
    item.appendChild(meta);
    return item;
  }

  function paint(suggestions) {
    if (!list || !empty) return;
    while (list.firstChild) {
      list.removeChild(list.firstChild);
    }
    if (!suggestions.length) {
      empty.hidden = false;
      list.hidden = true;
    } else {
      suggestions.forEach(function (suggestion) {
        list.appendChild(entry(suggestion));
      });
      empty.hidden = true;
      list.hidden = false;
      if (queueLink) {
        queueLink.textContent =
          "Review " + suggestions.length +
          (suggestions.length === 1 ? " pending suggestion" : " pending suggestions");
      }
    }
  }

  window
    .fetch("/api/timeline/" + encodeURIComponent(timelineId) + "/suggestions", {
      credentials: "same-origin",
      headers: { Accept: "application/json" }
    })
    .then(function (response) {
      return response.ok ? response.json() : null;
    })
    .then(function (suggestions) {
      if (Array.isArray(suggestions)) paint(suggestions);
    })
    .catch(function () {
      // The static empty state stays; nothing about a failed read is fatal.
    });
})();
