/*
 * Timelines site script.
 *
 * The shell loads this deferred on every page (static read pages and the API's
 * form/refusal pages alike). Pages are generated without knowing the viewer, so
 * every per-viewer affordance ships as anonymous markup and is upgraded here.
 *
 * Each section is an independent IIFE and must no-op on pages that do not carry
 * its markup, so sections can be added or changed without touching the others.
 * Nothing here is a source of truth: the API enforces every rule.
 */

/* --- account chrome (issue 05) --- */
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
      fetch("/api/timeline/" + encodeURIComponent(timelineId) + "/access", {
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

  fetch("/api/auth/me", {
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
/* --- end account chrome --- */
