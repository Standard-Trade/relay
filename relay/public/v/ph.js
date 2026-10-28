// PostHog for the hero test. The Worker puts the project key in a meta tag;
// with no key this only exposes a no-op sgTrack(). Every event carries the
// hero variant, and the page fires PostHog's standard exposure event so a
// PostHog Experiment on the `hero-copy` flag can read the results.
(function () {
  var variant = document.documentElement.dataset.variant || 'none';
  // Where this visit came from: the ?src= tag (/tt/<account> links set
  // tiktok-<account>), else the referring site, else direct.
  var q = new URLSearchParams(location.search);
  var source = q.get('src') || q.get('utm_source');
  if (!source) {
    try { var ref = document.referrer && new URL(document.referrer).hostname; source = ref && ref !== location.hostname ? ref : 'direct'; } catch (e) { source = 'direct'; }
  }
  window.sgSource = source;
  // Which AI answer engine sent the visit, if any (ChatGPT adds
  // utm_source=chatgpt.com; the library pages pass theirs on as ?src=).
  var ai = (source.match(/chatgpt|openai|perplexity|claude|gemini|copilot|bing\.com\/chat/) || [])[0] || '';
  if (ai === 'openai') ai = 'chatgpt';
  window.sgAiSrc = ai || 'none';
  var props = { hero_variant: variant, '$feature/hero-copy': variant, source: source, ai_src: ai || 'none' };
  window.sgTrack = function (ev, extra) {
    if (window.posthog && window.posthog.capture) window.posthog.capture(ev, Object.assign({}, props, extra || {}));
  };
  var meta = document.querySelector('meta[name="posthog-key"]');
  // Preview links (?hv=a|b|c) are for looking, not for the experiment.
  if (!meta || !meta.content || new URLSearchParams(location.search).has('hv')) return;
  // Sent through our own domain (/ingest, proxied by the Worker).
  var host = '/ingest';
  !function(t,e){var o,n,p,r;e.__SV||(window.posthog=e,e._i=[],e.init=function(i,s,a){function g(t,e){var o=e.split(".");2==o.length&&(t=t[o[0]],e=o[1]),t[e]=function(){t.push([e].concat(Array.prototype.slice.call(arguments,0)))}}(p=t.createElement("script")).type="text/javascript",p.crossOrigin="anonymous",p.async=!0,p.src=s.api_host.replace(".i.posthog.com","-assets.i.posthog.com")+"/static/array.js",(r=t.getElementsByTagName("script")[0]).parentNode.insertBefore(p,r);var u=e;for(void 0!==a?u=e[a]=[]:a="posthog",u.people=u.people||[],u.toString=function(t){var e="posthog";return"posthog"!==a&&(e+="."+a),t||(e+=" (stub)"),e},u.people.toString=function(){return u.toString(1)+".people (stub)"},o="init capture register register_once unregister opt_out_capturing has_opted_out_capturing opt_in_capturing reset isFeatureEnabled getFeatureFlag onFeatureFlags".split(" "),n=0;n<o.length;n++)g(u,o[n]);e._i.push([i,s,a])},e.__SV=1)}(document,window.posthog||[]);
  window.posthog.init(meta.content, { api_host: host, ui_host: 'https://us.posthog.com', person_profiles: 'identified_only', capture_pageview: true });
  window.posthog.register(props);
  window.posthog.register_once({ first_source: source, first_ai_src: ai || 'none' });
  window.posthog.capture('$feature_flag_called', { $feature_flag: 'hero-copy', $feature_flag_response: variant });
})();

// Taps on "text a reel" (the SMS onboarding test).
document.addEventListener('click', function (e) {
  var a = e.target.closest && e.target.closest('[data-text-cta]');
  if (!a || !window.sgTrack) return;
  var extra = { channel: a.getAttribute('data-text-cta') };
  if (a.getAttribute('data-inapp')) extra.inapp = a.getAttribute('data-inapp');
  window.sgTrack('text_cta_click', extra);
});
