// The site's only script file, shared by the landing page and the
// documentation pages. It sends nothing anywhere and loads nothing; every
// page reads and works without it.
(function () {
  "use strict";
  var root = document.documentElement;
  var calm = window.matchMedia && matchMedia("(prefers-reduced-motion: reduce)").matches;

  // Blocks marked .reveal fade in when they scroll into view.
  var blocks = document.querySelectorAll(".reveal");
  if (blocks.length && "IntersectionObserver" in window && !calm) {
    root.classList.add("js");
    var seen = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (entry.isIntersecting) {
          entry.target.classList.add("in");
          seen.unobserve(entry.target);
        }
      });
    }, { rootMargin: "0px 0px -8% 0px", threshold: 0.08 });
    blocks.forEach(function (block) { seen.observe(block); });
  }

  // The demo film: a Pause / Play button, and no playing off screen.
  var demo = document.getElementById("demo");
  var toggle = document.getElementById("demo-toggle");
  if (demo && toggle && demo.play) {
    var stopped = calm;                 // the visitor's own choice, kept while scrolling
    var show = function () {
      toggle.textContent = demo.paused ? "Play" : "Pause";
      toggle.classList.toggle("paused", demo.paused);
    };
    toggle.hidden = false;
    toggle.addEventListener("click", function () {
      stopped = !demo.paused;
      if (demo.paused) demo.play(); else demo.pause();
    });
    demo.addEventListener("play", show);
    demo.addEventListener("pause", show);
    show();
    if ("IntersectionObserver" in window) {
      new IntersectionObserver(function (entries) {
        if (stopped) return;
        if (entries[0].isIntersecting) { var p = demo.play(); if (p && p.catch) p.catch(function () {}); }
        else demo.pause();
      }, { threshold: 0.1 }).observe(demo);
    }
  }

  // Documentation pages: mark the section being read in "On this page".
  var links = document.querySelectorAll(".docs-toc a");
  if (links.length && "IntersectionObserver" in window) {
    var byId = {};
    links.forEach(function (a) { byId[decodeURIComponent(a.getAttribute("href").slice(1))] = a; });
    var current = null;
    var spy = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        if (current) current.classList.remove("here");
        current = byId[entry.target.id];
        if (current) current.classList.add("here");
      });
    }, { rootMargin: "-72px 0px -70% 0px" });
    document.querySelectorAll(".doc h2[id], .doc h3[id]").forEach(function (h) { spy.observe(h); });
  }
})();
