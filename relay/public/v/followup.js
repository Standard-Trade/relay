// Two-tap follow-up after a waitlist sign-up: what they're building, and
// which chat app they'd join a community on. Styled from the page's own
// colors via currentColor, so it fits every variant.
window.sgFollowup = function (afterEl, email) {
  const steps = [
    { key: 'business', q: 'What are you building?', opts: [['store', 'Online store'], ['app', 'App'], ['newsletter', 'Newsletter or media'], ['creator', 'Creator brand'], ['service', 'Service or agency'], ['other', 'Something else']] },
    { key: 'community', q: 'If we start a community, where would you join?', opts: [['discord', 'Discord'], ['slack', 'Slack'], ['telegram', 'Telegram'], ['whatsapp', 'WhatsApp'], ['none', 'No thanks']] },
  ];
  const box = document.createElement('div');
  box.className = 'sg-follow';
  box.innerHTML = '<style>.sg-follow{margin-top:18px}.sg-follow p{margin:0 0 10px;font-size:15px;font-weight:600}.sg-follow .chips{display:flex;flex-wrap:wrap;gap:8px}.sg-follow button{font:inherit;font-size:14px;padding:8px 13px;border-radius:999px;border:1px solid currentColor;background:transparent;color:inherit;cursor:pointer;opacity:.85;transition:opacity .15s}.sg-follow button:hover{opacity:1}.sg-follow button:focus-visible{outline:2px solid currentColor;outline-offset:2px}.sg-follow .done{font-weight:400;opacity:.8}</style>';
  afterEl.insertAdjacentElement('afterend', box);
  let i = 0;
  const show = () => {
    if (i >= steps.length) { box.insertAdjacentHTML('beforeend', '<p class="done">Thanks. That helps us build the right thing.</p>'); return; }
    const st = steps[i];
    const wrap = document.createElement('div');
    wrap.innerHTML = '<p></p><div class="chips" role="group"></div>';
    wrap.querySelector('p').textContent = st.q;
    wrap.querySelector('.chips').setAttribute('aria-label', st.q);
    st.opts.forEach(([val, label]) => {
      const b = document.createElement('button');
      b.type = 'button'; b.textContent = label;
      b.onclick = () => {
        if (window.sgTrack) window.sgTrack('waitlist_profile', { [st.key]: val });
        fetch('/waitlist/profile', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email, [st.key]: val }) }).catch(() => {});
        wrap.remove(); i += 1; show();
      };
      wrap.querySelector('.chips').appendChild(b);
    });
    box.appendChild(wrap);
    wrap.querySelector('button').focus({ preventScroll: true });
  };
  show();
};
