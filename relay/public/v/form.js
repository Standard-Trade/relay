// Shared waitlist form for the hero variants. Each page sets data-variant on <form>.
document.querySelectorAll('form[data-variant]').forEach((f) => {
  const scope = f.closest('[data-form-scope]') || f.parentElement;
  const err = scope.querySelector('.err');
  const btn = f.querySelector('button');
  const src = (window.sgSource || new URLSearchParams(location.search).get('src') || 'direct') + '|' + f.dataset.variant;
  f.addEventListener('submit', async (e) => {
    e.preventDefault();
    err.textContent = '';
    const email = f.email.value.trim();
    if (!/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(email)) { err.textContent = 'Enter an email we can reach you at.'; f.email.focus(); return; }
    btn.disabled = true; btn.dataset.label = btn.textContent; btn.textContent = 'Saving…';
    try {
      const r = await fetch('/waitlist', { method: 'POST', headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ email, site: f.extra ? f.extra.value.trim() : '', src, company: f.company.value,
          ph: window.posthog && typeof window.posthog.get_distinct_id === 'function' ? window.posthog.get_distinct_id() : '' }) });
      const j = await r.json().catch(() => ({}));
      if (!r.ok) throw new Error(j.error || 'That didn\'t go through. Try again.');
      if (window.sgTrack) window.sgTrack('waitlist_signup', { source: src });
      f.hidden = true; const ok = scope.querySelector('.ok'); ok.hidden = false; if (window.sgFollowup) window.sgFollowup(ok, email);
    } catch (e2) { err.textContent = e2.message; btn.disabled = false; btn.textContent = btn.dataset.label; }
  });
});
