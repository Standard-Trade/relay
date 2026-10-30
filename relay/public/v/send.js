// The hero's main action: send us a reel over iMessage or WhatsApp, with
// the message prefilled. iPhone and Mac default to iMessage, Android to
// WhatsApp; the switch flips it. Email stays as a quiet fallback.
(function () {
  var NUM = '16287261402';
  // A referral link (?ref=rf_<12 hex>) carries its code into the first
  // text, which is where Standard Grow reads it. Anything else is dropped.
  // "Text me" sends the same code with the phone number.
  var REF = (new URLSearchParams(location.search).get('ref') || '').toLowerCase();
  if (!/^rf_[0-9a-f]{12}$/.test(REF)) REF = '';
  var MSG = 'Hey, help me grow my business' + (REF ? ' (ref ' + REF + ')' : '');
  var APPS = {
    imessage: { label: 'iMessage', href: 'sms:+' + NUM + '&body=' + encodeURIComponent(MSG),
      icon: '<svg width="24" height="24" viewBox="0 0 24 24" aria-hidden="true"><path d="M5.285 0A5.273 5.273 0 0 0 0 5.285v13.43A5.273 5.273 0 0 0 5.285 24h13.43A5.273 5.273 0 0 0 24 18.715V5.285A5.273 5.273 0 0 0 18.715 0ZM12 4.154a8.809 7.337 0 0 1 8.809 7.338A8.809 7.337 0 0 1 12 18.828a8.809 7.337 0 0 1-2.492-.303A8.656 7.337 0 0 1 5.93 19.93a9.929 7.337 0 0 0 1.54-2.155 8.809 7.337 0 0 1-4.279-6.283A8.809 7.337 0 0 1 12 4.154" fill="#34C759"/></svg>' },
    whatsapp: { label: 'WhatsApp', href: 'https://wa.me/' + NUM + '?text=' + encodeURIComponent(MSG),
      icon: '<svg width="24" height="24" viewBox="0 0 24 24" aria-hidden="true"><path d="M17.472 14.382c-.297-.149-1.758-.867-2.03-.967-.273-.099-.471-.148-.67.15-.197.297-.767.966-.94 1.164-.173.199-.347.223-.644.075-.297-.15-1.255-.463-2.39-1.475-.883-.788-1.48-1.761-1.653-2.059-.173-.297-.018-.458.13-.606.134-.133.298-.347.446-.52.149-.174.198-.298.298-.497.099-.198.05-.371-.025-.52-.075-.149-.669-1.612-.916-2.207-.242-.579-.487-.5-.669-.51-.173-.008-.371-.01-.57-.01-.198 0-.52.074-.792.372-.272.297-1.04 1.016-1.04 2.479 0 1.462 1.065 2.875 1.213 3.074.149.198 2.096 3.2 5.077 4.487.709.306 1.262.489 1.694.625.712.227 1.36.195 1.871.118.571-.085 1.758-.719 2.006-1.413.248-.694.248-1.289.173-1.413-.074-.124-.272-.198-.57-.347m-5.421 7.403h-.004a9.87 9.87 0 01-5.031-1.378l-.361-.214-3.741.982.998-3.648-.235-.374a9.86 9.86 0 01-1.51-5.26c.001-5.45 4.436-9.884 9.888-9.884 2.64 0 5.122 1.03 6.988 2.898a9.825 9.825 0 012.893 6.994c-.003 5.45-4.437 9.884-9.885 9.884m8.413-18.297A11.815 11.815 0 0012.05 0C5.495 0 .16 5.335.157 11.892c0 2.096.547 4.142 1.588 5.945L.057 24l6.305-1.654a11.882 11.882 0 005.683 1.448h.005c6.554 0 11.89-5.335 11.893-11.893a11.821 11.821 0 00-3.48-8.413Z" fill="#25D366"/></svg>' }
  };
  // White marks for the colored button; the brand-colored icons sit in the switch.
  var WHITE = {
    imessage: '<svg width="24" height="24" viewBox="0 0 24 24" aria-hidden="true"><path d="M12 4.154a8.809 7.337 0 0 1 8.809 7.338A8.809 7.337 0 0 1 12 18.828a8.809 7.337 0 0 1-2.492-.303A8.656 7.337 0 0 1 5.93 19.93a9.929 7.337 0 0 0 1.54-2.155 8.809 7.337 0 0 1-4.279-6.283A8.809 7.337 0 0 1 12 4.154" fill="#fff"/></svg>',
    whatsapp: '<svg width="24" height="24" viewBox="0 0 24 24" aria-hidden="true"><path d="M17.472 14.382c-.297-.149-1.758-.867-2.03-.967-.273-.099-.471-.148-.67.15-.197.297-.767.966-.94 1.164-.173.199-.347.223-.644.075-.297-.15-1.255-.463-2.39-1.475-.883-.788-1.48-1.761-1.653-2.059-.173-.297-.018-.458.13-.606.134-.133.298-.347.446-.52.149-.174.198-.298.298-.497.099-.198.05-.371-.025-.52-.075-.149-.669-1.612-.916-2.207-.242-.579-.487-.5-.669-.51-.173-.008-.371-.01-.57-.01-.198 0-.52.074-.792.372-.272.297-1.04 1.016-1.04 2.479 0 1.462 1.065 2.875 1.213 3.074.149.198 2.096 3.2 5.077 4.487.709.306 1.262.489 1.694.625.712.227 1.36.195 1.871.118.571-.085 1.758-.719 2.006-1.413.248-.694.248-1.289.173-1.413-.074-.124-.272-.198-.57-.347m-5.421 7.403h-.004a9.87 9.87 0 01-5.031-1.378l-.361-.214-3.741.982.998-3.648-.235-.374a9.86 9.86 0 01-1.51-5.26c.001-5.45 4.436-9.884 9.888-9.884 2.64 0 5.122 1.03 6.988 2.898a9.825 9.825 0 012.893 6.994c-.003 5.45-4.437 9.884-9.885 9.884m8.413-18.297A11.815 11.815 0 0012.05 0C5.495 0 .16 5.335.157 11.892c0 2.096.547 4.142 1.588 5.945L.057 24l6.305-1.654a11.882 11.882 0 005.683 1.448h.005c6.554 0 11.89-5.335 11.893-11.893a11.821 11.821 0 00-3.48-8.413Z" fill="#fff"/></svg>'
  };
  var android = /Android/i.test(navigator.userAgent);
  // In-app browsers (TikTok, Instagram, Facebook) block sms: and app
  // schemes, so the button does nothing there. In those the switch still
  // shows, but a tap opens a sheet: with iMessage picked it leads with the hop
  // to the real browser, with WhatsApp picked it leads with the https link.
  var ua = navigator.userAgent;
  var inapp = /musical_ly|BytedanceWebview|TikTok|trill_/i.test(ua) ? 'TikTok'
    : /Instagram/i.test(ua) ? 'Instagram'
    : /FBAN|FBAV|FB_IAB/i.test(ua) ? 'Facebook' : '';
  var browser = android ? 'Chrome' : 'Safari';
  // Navigation goes through one function so tests can watch it.
  function nav(u) { if (window.sgNavigate) window.sgNavigate(u); else location.href = u; }
  // This page in the real browser, with ?go=<app> so it picks up where the
  // visitor left off. The hero variant and source ride along (the cookie
  // that holds them stays behind in the in-app browser).
  function outUrl(app) {
    var u = new URL(location.href);
    u.searchParams.set('go', app);
    if (!u.searchParams.has('hv') && document.documentElement.dataset.variant) u.searchParams.set('hv', document.documentElement.dataset.variant);
    if (!u.searchParams.has('src') && window.sgSource) u.searchParams.set('src', window.sgSource);
    return u.toString();
  }
  // Hand the page to the real browser. Per inapp-debugger (2026): Facebook
  // iOS honors x-safari-https via window.open, Instagram iOS its own
  // extbrowser link, and Android Instagram/Facebook an intent:// for Chrome.
  // TikTok blocks all of these, so the sheet below is the fallback there.
  function escapeUrl(app) {
    var u = outUrl(app);
    if (android) {
      var rest = u.replace(/^https?:\/\//, '');
      return { method: 'intent', url: 'intent://' + rest + '#Intent;scheme=https;package=com.android.chrome;S.browser_fallback_url=' + encodeURIComponent(u) + ';end' };
    }
    if (inapp === 'Instagram') return { method: 'instagram', url: 'instagram://extbrowser/?url=' + encodeURIComponent(u) };
    return { method: inapp === 'Facebook' ? 'x-safari-open' : 'x-safari', url: 'x-safari-' + u.replace(/^http:/, 'https:') };
  }
  // Try to leave; if the page is still showing after a beat, open the sheet.
  function tryEscape(app, fallback) {
    var e = escapeUrl(app), left = false;
    function gone() { left = true; }
    document.addEventListener('visibilitychange', function () { if (document.visibilityState === 'hidden') gone(); }, { once: true });
    window.addEventListener('pagehide', gone, { once: true });
    if (window.sgTrack) window.sgTrack('inapp_escape_try', { inapp: inapp, method: e.method, app: app });
    try {
      if (e.method === 'x-safari-open' && !window.sgNavigate) window.open(e.url, '_blank');
      else nav(e.url);
    } catch (err) {}
    setTimeout(function () {
      if (left) return;
      if (window.sgTrack) window.sgTrack('inapp_escape_fallback', { inapp: inapp, method: e.method, app: app });
      fallback();
    }, window.sgEscapeWait || 1200);
  }
  // "Text me" in the sheet (phone in, we text first). On by default;
  // ?textme=0 or <html data-textme="0"> hides it for testing.
  var textme = document.documentElement.dataset.textme !== '0' && new URLSearchParams(location.search).get('textme') !== '0';
  function sheet(mode) {
    var s = document.querySelector('.send-sheet');
    if (s) return order(s, mode);
    s = document.createElement('div');
    s.className = 'send-sheet';
    s.hidden = true;
    s.innerHTML =
      '<div class="send-sheet-card" role="dialog" aria-modal="true" aria-labelledby="send-sheet-t">' +
      '<p class="send-sheet-t" id="send-sheet-t"></p>' +
      '<a class="send-go" data-app="whatsapp" data-sheet="whatsapp" target="_blank" rel="noopener" href="' + APPS.whatsapp.href + '">' + WHITE.whatsapp + '<span>Text us on WhatsApp</span></a>' +
      '<button type="button" class="send-sheet-copy" data-sheet="copy">Copy link for ' + browser + '</button>' +
      '<p class="send-sheet-hint">Or tap \u00b7\u00b7\u00b7 and choose Open in browser.</p>' +
      (textme ? '<form class="send-textme" novalidate><label for="send-textme-p">Or we text you first</label><div><input id="send-textme-p" type="tel" name="phone" autocomplete="tel" inputmode="tel" placeholder="Your phone number" required><input class="hp" name="company" tabindex="-1" autocomplete="off" aria-hidden="true"><button type="submit">Text me</button></div><p class="send-textme-msg" role="status"></p></form>' : '') +
      '<button type="button" class="send-sheet-x" aria-label="Close">\u00d7</button></div>';
    document.body.appendChild(s);
    function close() { s.hidden = true; }
    s.onclick = function (e) { if (e.target === s) close(); };
    s.querySelector('.send-sheet-x').onclick = close;
    s.querySelector('[data-sheet="whatsapp"]').onclick = function () { if (window.sgTrack) window.sgTrack('inapp_sheet_whatsapp', { inapp: inapp }); };
    s.querySelector('[data-sheet="copy"]').onclick = function () {
      var b = this, url = location.href;
      if (window.sgTrack) window.sgTrack('inapp_sheet_copy', { inapp: inapp });
      function done() { b.textContent = 'Copied. Paste it in ' + browser + '.'; }
      if (navigator.clipboard && navigator.clipboard.writeText) navigator.clipboard.writeText(url).then(done, function () { window.prompt('Copy this link', url); });
      else window.prompt('Copy this link', url);
    };
    document.addEventListener('keydown', function (e) { if (e.key === 'Escape') close(); });
    var tf = s.querySelector('.send-textme');
    if (tf) tf.onsubmit = function (e) {
      e.preventDefault();
      var msg = tf.querySelector('.send-textme-msg'), btn = tf.querySelector('button'), phone = tf.phone.value.trim();
      if (phone.replace(/\D/g, '').length < 10) { msg.textContent = 'Enter a phone number with area code.'; tf.phone.focus(); return; }
      btn.disabled = true; msg.textContent = '';
      fetch('/api/textme', { method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ phone: phone, company: tf.company.value, src: (window.sgSource || 'direct') + '|' + (document.documentElement.dataset.variant || ''), inapp: inapp, ref: REF,
          ph: window.posthog && typeof window.posthog.get_distinct_id === 'function' ? window.posthog.get_distinct_id() : '' }) })
        .then(function (r) { return r.json().then(function (j) { if (!r.ok) throw new Error(j.error || 'That didn\u2019t go through. Try again.'); }); })
        .then(function () {
          if (window.sgTrack) window.sgTrack('textme_submitted', { inapp: inapp });
          tf.innerHTML = '<p class="send-textme-ok">Got it. We\u2019ll text you shortly.</p>';
        }, function (err) { msg.textContent = err.message; btn.disabled = false; });
    };
    return order(s, mode);
  }
  function order(s, mode) {
    var card = s.querySelector('.send-sheet-card'), wa = s.querySelector('[data-sheet="whatsapp"]'),
      copy = s.querySelector('[data-sheet="copy"]'), hint = s.querySelector('.send-sheet-hint');
    s.querySelector('.send-sheet-t').textContent = inapp + '\u2019s browser can\u2019t open ' + (mode === 'imessage' ? 'Messages' : 'WhatsApp');
    var tm = s.querySelector('.send-textme');
    (mode === 'imessage' ? [copy, hint, wa, tm] : [wa, copy, hint, tm]).forEach(function (el) { if (el) card.insertBefore(el, s.querySelector('.send-sheet-x')); });
    copy.classList.toggle('send-sheet-primary', mode === 'imessage');
    s.dataset.mode = mode;
    return s;
  }
  document.querySelectorAll('[data-form-scope]').forEach(function (scope) {
    var form = scope.querySelector('form');
    if (!form || scope.querySelector('.send')) return;
    var old = scope.querySelector('.textcta');
    if (old) old.remove();
    var box = document.createElement('div');
    box.className = 'send';
    box.innerHTML =
      '<div class="send-switch" role="group" aria-label="Choose an app">' +
      ['imessage', 'whatsapp'].map(function (k) { return '<button type="button" data-pick="' + k + '">' + APPS[k].icon.replace('width="24" height="24"', 'width="16" height="16"') + APPS[k].label + '</button>'; }).join('') +
      '</div><a class="send-go" data-text-cta></a>' +
      
      '<p class="send-email"><button type="button">Not ready to text? Leave your email instead</button></p>';
    form.parentNode.insertBefore(box, form);
    form.setAttribute('data-send-form', '');
    form.hidden = true;
    box.querySelector('.send-email button').onclick = function () {
      form.hidden = false; this.parentNode.remove();
      var i = form.querySelector('input[type=email]'); if (i) i.focus();
      if (window.sgTrack) window.sgTrack('email_fallback_open');
    };
    function pick(k) {
      var a = box.querySelector('.send-go');
      // sms: never works in an in-app browser; there the tap opens the sheet,
      // and the href stays the https WhatsApp link.
      a.href = inapp ? APPS.whatsapp.href : APPS[k].href; a.dataset.app = k; a.setAttribute('data-text-cta', k);
      a.target = k === 'whatsapp' ? '_blank' : ''; a.rel = 'noopener';
      a.innerHTML = WHITE[k] + '<span>Get my first growth test</span>';
      box.querySelectorAll('[data-pick]').forEach(function (b) { b.setAttribute('aria-pressed', String(b.dataset.pick === k)); });
    }
    box.querySelectorAll('[data-pick]').forEach(function (b) { b.onclick = function () { pick(b.dataset.pick); }; });
    pick(android ? 'whatsapp' : 'imessage');
    if (inapp) {
      var go = box.querySelector('.send-go');
      go.setAttribute('data-inapp', inapp);
      go.onclick = function (e) {
        e.preventDefault();
        var app = go.dataset.app;
        // The escape has to start inside the tap; scripted redirects outside
        // a user gesture get blocked.
        tryEscape(app, function () {
          var s = sheet(app); s.hidden = false;
          s.querySelector(app === 'imessage' ? '[data-sheet="copy"]' : '[data-sheet="whatsapp"]').focus();
        });
      };
    } else {
      // Arrived from an in-app browser (?go=<app>): pick that app, drop the
      // parameter, bring the button into view and try to open the app. iOS
      // may block an sms: that no tap started, so the button stays ready.
      var params = new URLSearchParams(location.search), want = params.get('go');
      if (want === 'imessage' || want === 'whatsapp') {
        params.delete('go');
        var q = params.toString();
        try { history.replaceState(null, '', location.pathname + (q ? '?' + q : '') + location.hash); } catch (err) {}
        pick(want);
        var btn = box.querySelector('.send-go');
        btn.classList.add('send-go-arrived');
        btn.scrollIntoView({ block: 'center' });
        if (window.sgTrack) window.sgTrack('inapp_escape_arrived', { app: want });
        if (want === 'imessage') setTimeout(function () { nav(APPS.imessage.href); }, 300);
      }
    }
  });
})();
