// Meta pixel for the ads. The Worker puts the pixel id in a meta tag; with no
// id this only exposes sgMeta(), which does nothing. Each event carries an
// event id, and the Worker sends Meta the same event from the server
// (Conversions API) with that id, so Meta counts it once.
(function () {
  function newId() { return 'e' + Date.now().toString(36) + Math.random().toString(36).slice(2, 10); }
  window.sgMetaId = newId;
  window.sgMeta = function (ev, id) { if (window.fbq) window.fbq('track', ev, {}, { eventID: id }); };
  var meta = document.querySelector('meta[name="meta-pixel"]');
  if (!meta || !meta.content) return;
  !function(f,b,e,v,n,t,s){if(f.fbq)return;n=f.fbq=function(){n.callMethod?n.callMethod.apply(n,arguments):n.queue.push(arguments)};if(!f._fbq)f._fbq=n;n.push=n;n.loaded=!0;n.version='2.0';n.queue=[];t=b.createElement(e);t.async=!0;t.src=v;s=b.getElementsByTagName(e)[0];s.parentNode.insertBefore(t,s)}(window,document,'script','https://connect.facebook.net/en_US/fbevents.js');
  window.fbq('init', meta.content);
  window.fbq('track', 'PageView');
  // A tap on "Text Grow" is the sign-up the ads optimize for. The page is
  // about to leave for Messages or WhatsApp, so the server copy goes by beacon.
  document.addEventListener('click', function (e) {
    if (!e.target.closest || !e.target.closest('[data-text-cta]')) return;
    var id = newId();
    window.sgMeta('Lead', id);
    var body = JSON.stringify({ event: 'Lead', id: id, url: location.href });
    if (navigator.sendBeacon) navigator.sendBeacon('/api/meta', new Blob([body], { type: 'application/json' }));
    else fetch('/api/meta', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: body, keepalive: true });
  });
})();
