// Landing page checks for the hero variants in ../relay/public/h, run before
// every deploy (`npm test`, and `npm run deploy` runs it first).
//
// 1. Mobile layout: at phone widths, no horizontal scroll, every visible image
//    sits inside the viewport, and every image group (a container holding two
//    or more images, like variant b's three slides) is horizontally centered.
// 2. Text CTA: in TikTok / Instagram / Facebook in-app browsers the button
//    must not depend on sms: (those webviews block it); in Safari it stays sms:.
// 3. Answer engines: each hero has a visible FAQ that fits at phone widths, a
//    meta description, and JSON-LD that parses and matches the visible FAQ;
//    robots.txt, sitemap.xml and the IndexNow key file are served.
//
// Usage: node tests/landing.test.mjs [--shots <dir>]
import { chromium } from 'playwright';
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../relay/public');
const VARIANTS = ['a', 'b', 'c'];
const WIDTHS = [320, 360, 375, 390, 414, 430];
const TOL = 1; // px
const shotsArg = process.argv.indexOf('--shots');
const SHOTS = shotsArg > 0 ? process.argv[shotsArg + 1] : null;

const UA = {
  safari: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1',
  chromeAndroid: 'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/129.0.0.0 Mobile Safari/537.36',
  tiktokIos: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 musical_ly_37.5.0 JsSdk/2.0 NetType/WIFI Channel/App Store ByteLocale/en Region/US ByteFullLocale/en isDarkMode/0 WKWebView/1 RevealType/Dialog BytedanceWebview/d8a21c6',
  tiktokAndroid: 'Mozilla/5.0 (Linux; Android 14; Pixel 8 Build/AP2A.240805.005; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/129.0.6668.100 Mobile Safari/537.36 trill_370504 JsSdk/1.0 NetType/WIFI Channel/googleplay AppName/musical_ly app_version/37.5.4 ByteLocale/en ByteFullLocale/en Region/US BytedanceWebview/d8a21c6',
  instagram: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 Instagram 350.0.0.25.81 (iPhone15,2; iOS 18_5; en_US; en; scale=3.00; 1179x2556; 640000000)',
  facebook: 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 [FBAN/FBIOS;FBAV/480.0.0.40.98;FBBV/650000000;FBDV/iPhone15,2;FBMD/iPhone;FBSN/iOS;FBSV/18.5;FBSS/3;FBID/phone;FBLC/en_US;FBOP/5]',
};

const TYPES = { '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript', '.png': 'image/png', '.jpg': 'image/jpeg', '.woff2': 'font/woff2' };
function serve() {
  const server = http.createServer((req, res) => {
    let p = decodeURIComponent(new URL(req.url, 'http://x').pathname);
    let file = path.join(ROOT, p);
    if (!file.startsWith(ROOT)) { res.writeHead(403); return res.end(); }
    if (!path.extname(file) && fs.existsSync(file + '.html')) file += '.html';
    fs.readFile(file, (err, buf) => {
      if (err) { res.writeHead(404); return res.end(); }
      res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] || 'application/octet-stream' });
      res.end(buf);
    });
  });
  return new Promise(r => server.listen(0, '127.0.0.1', () => r(server)));
}

const failures = [];
let checks = 0;
function check(ok, msg) { checks++; if (!ok) failures.push(msg); }

async function page(browser, base, variant, width, ua = UA.safari, query = '') {
  const ctx = await browser.newContext({ viewport: { width, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, userAgent: ua });
  // Only our own origin: Google Fonts and PostHog would make runs flaky.
  await ctx.route('**/*', r => r.request().url().startsWith(base) ? r.continue() : r.abort());
  // Record navigations send.js makes (escape links, sms:) instead of following them.
  await ctx.addInitScript(() => { window.__navs = []; window.sgNavigate = u => window.__navs.push(u); window.sgEscapeWait = 150; });
  const p = await ctx.newPage();
  await p.goto(`${base}/h/${variant}${query}`, { waitUntil: 'load' });
  await p.evaluate(() => document.fonts.ready);
  await p.evaluate(() => Promise.all([...document.images].map(i => i.complete ? 0 : new Promise(r => { i.onload = i.onerror = r; }))));
  return { ctx, p };
}

// vw is the device width we asked for, not innerWidth: mobile browsers widen
// the layout viewport to fit overflowing content, which would hide the bug.
function measure(vw) {
  const visible = el => {
    const s = getComputedStyle(el), r = el.getBoundingClientRect();
    return s.display !== 'none' && s.visibility !== 'hidden' && r.width > 0 && r.height > 0 && !el.closest('[hidden]');
  };
  const label = el => el.tagName.toLowerCase() + (el.className && typeof el.className === 'string' ? '.' + el.className.trim().split(/\s+/).join('.') : '') + (el.getAttribute('src') ? `[src=${el.getAttribute('src')}]` : '');
  const media = [...document.querySelectorAll('img, picture, video, svg')].filter(visible).filter(el => !el.parentElement.closest('svg'));
  const items = media.map(el => { const r = el.getBoundingClientRect(); return { el: label(el), left: r.left, right: vw - r.right }; });
  // Image groups: a row/stack of images, i.e. the nearest ancestor (within
  // three levels) with two or more visible children that are all images or
  // text-free image wrappers (a logo next to a name is not a group).
  const isMedia = el => /^(IMG|PICTURE|VIDEO)$/.test(el.tagName);
  const mediaOnly = el => isMedia(el) || (el.querySelector('img, picture, video') && !el.innerText.trim());
  const groups = new Map();
  media.filter(isMedia).forEach(el => {
    let g = el.parentElement;
    for (let i = 0; i < 3 && g && g !== document.body; i++, g = g.parentElement) {
      const kids = [...g.children].filter(visible);
      if (kids.length >= 2 && kids.every(mediaOnly)) { if (!groups.has(g)) groups.set(g, []); groups.get(g).push(el); return; }
    }
  });
  const groupRows = [...groups].map(([g, els]) => {
    const rs = els.map(e => e.getBoundingClientRect());
    const left = Math.min(...rs.map(r => r.left)), right = Math.max(...rs.map(r => r.right));
    const gr = g.getBoundingClientRect(), cs = getComputedStyle(g);
    const cl = gr.left + parseFloat(cs.paddingLeft) + parseFloat(cs.borderLeftWidth);
    const cr = gr.right - parseFloat(cs.paddingRight) - parseFloat(cs.borderRightWidth);
    return { el: label(g), n: els.length, vpLeft: left, vpRight: vw - right, boxLeft: left - cl, boxRight: cr - right };
  });
  return { vw, innerWidth: window.innerWidth, scrollWidth: document.documentElement.scrollWidth, bodyScroll: document.body.scrollWidth, items, groups: groupRows };
}

async function layout(browser, base) {
  for (const v of VARIANTS) for (const w of WIDTHS) {
    const { ctx, p } = await page(browser, base, v, w);
    const m = await p.evaluate(measure, w);
    const where = `[h/${v} @${w}px]`;
    check(m.innerWidth === m.vw && m.scrollWidth <= m.vw && m.bodyScroll <= m.vw, `${where} horizontal overflow: layout width ${m.innerWidth}, scrollWidth ${m.scrollWidth}/${m.bodyScroll}, device ${m.vw}`);
    for (const it of m.items) check(it.left >= -TOL && it.right >= -TOL, `${where} ${it.el} outside viewport (left ${it.left.toFixed(1)}, right ${it.right.toFixed(1)})`);
    for (const g of m.groups) {
      check(Math.abs(g.vpLeft - g.vpRight) <= TOL, `${where} group ${g.el} (${g.n} images) off-center in viewport: left gap ${g.vpLeft.toFixed(1)} vs right gap ${g.vpRight.toFixed(1)}`);
      check(Math.abs(g.boxLeft - g.boxRight) <= TOL, `${where} group ${g.el} off-center in its container: left gap ${g.boxLeft.toFixed(1)} vs right gap ${g.boxRight.toFixed(1)}`);
    }
    const faq = await p.evaluate(vw => {
      const f = document.querySelector('section.faq');
      if (!f) return null;
      const els = [f, ...f.querySelectorAll('*')].map(e => e.getBoundingClientRect());
      return { h: f.getBoundingClientRect().height, left: Math.min(...els.map(r => r.left)), right: Math.max(...els.map(r => r.right)), vw, n: f.querySelectorAll('.faq-i').length };
    }, w);
    check(!!faq && faq.h > 0 && faq.n > 0, `${where} FAQ section missing or empty`);
    if (faq) check(faq.left >= -TOL && faq.right <= w + TOL, `${where} FAQ overflows: left ${faq.left.toFixed(1)}, right ${faq.right.toFixed(1)} of ${w}`);
    if (SHOTS) {
      fs.mkdirSync(SHOTS, { recursive: true });
      await p.screenshot({ path: path.join(SHOTS, `h${v}-${w}.png`), fullPage: true });
      const slides = await p.$('.slides');
      if (slides && w === 375) await slides.screenshot({ path: path.join(SHOTS, `h${v}-${w}-slides.png`) });
    }
    await ctx.close();
  }
}

async function cta(browser, base) {
  // [user agent, expected href, in-app browser, app picked by default, expected escape link]
  const cases = [
    ['safari', /^sms:/, false, 'imessage'],
    ['chromeAndroid', /^https:\/\/wa\.me\//, false, 'whatsapp'],
    ['tiktokIos', /^https:\/\//, true, 'imessage', /^x-safari-https:\/\/[^#]*[?&]go=/],
    ['tiktokAndroid', /^https:\/\//, true, 'whatsapp', /^intent:\/\/[^#]*[?&]go=[^#]*#Intent;scheme=https;package=com\.android\.chrome;S\.browser_fallback_url=[^;]+;end$/],
    ['instagram', /^https:\/\//, true, 'imessage', /^instagram:\/\/extbrowser\/\?url=https?%3A%2F%2F.*go%3D/],
    ['facebook', /^https:\/\//, true, 'imessage', /^x-safari-https:\/\/[^#]*[?&]go=/],
  ];
  const state = () => {
    const a = document.querySelector('.send a.send-go'), sw = document.querySelector('.send-switch');
    return { href: a && a.getAttribute('href'), app: a && a.dataset.app, text: a && a.textContent.trim(),
      next: a && a.nextElementSibling && a.nextElementSibling.className, body: document.body.innerText,
      switchShown: !!sw && !sw.hidden && getComputedStyle(sw).display !== 'none' };
  };
  for (const v of VARIANTS) for (const [ua, want, inapp, def, escape] of cases) {
    const { ctx, p } = await page(browser, base, v, 390, UA[ua]);
    const where = `[h/${v} ${ua}]`;
    const btns = await p.$$('.send a.send-go');
    check(btns.length === 1, `${where} expected one CTA button, found ${btns.length}`);
    const info = await p.evaluate(state);
    check(info.switchShown, `${where} iMessage/WhatsApp switch should be visible`);
    check(info.app === def, `${where} default app is ${info.app}, want ${def}`);
    check(want.test(info.href || ''), `${where} CTA href ${info.href} should match ${want}`);
    check(info.text === 'Get my first growth test', `${where} CTA label is "${info.text}"`);
    if (!inapp) check(decodeURIComponent(info.href || '').includes('Hey, help me grow my business'), `${where} CTA is missing the prefilled message`);
    check(!/702\D?350\D?8103/.test(info.body), `${where} phone number is visible on the page`);
    check(info.next !== 'send-note', `${where} helper text under the CTA`);
    if (inapp) {
      // Tapping must keep the visitor on the page and offer ways out, never fire sms:.
      const navs = [];
      p.on('framenavigated', f => { if (f === p.mainFrame()) navs.push(f.url()); });
      for (const app of ['imessage', 'whatsapp']) {
        await p.click(`[data-pick="${app}"]`);
        const now = await p.evaluate(state);
        check(/^https:/.test(now.href || ''), `${where} ${app}: in-app CTA href ${now.href} must be https`);
        await p.evaluate(() => { window.__navs.length = 0; });
        await p.click('.send a.send-go');
        // First the tap tries to hand the page to the real browser...
        const navsNow = await p.evaluate(() => window.__navs.slice());
        check(navsNow.length === 1 && escape.test(navsNow[0]), `${where} ${app}: tap should try escape ${escape}, got ${navsNow}`);
        check(navsNow.length === 1 && (navsNow[0].includes('go%3D' + app) || navsNow[0].includes('go=' + app)), `${where} ${app}: escape link should carry go=${app}`);
        check(navsNow.length === 1 && (navsNow[0].includes('hv%3D' + v) || navsNow[0].includes('hv=' + v)), `${where} ${app}: escape link should keep hero variant ${v}`);
        check(!(await p.evaluate(() => { const s = document.querySelector('.send-sheet'); return s && !s.hidden; })), `${where} ${app}: sheet opened before the escape had a chance`);
        // ...and only if the page is still showing does the sheet open.
        await p.waitForTimeout(300);
        const sheet = await p.evaluate(() => {
          const s = document.querySelector('.send-sheet');
          if (!s || s.hidden) return null;
          const first = s.querySelector('.send-sheet-card > :not(.send-sheet-t):not(.send-sheet-x)');
          return { links: [...s.querySelectorAll('a[href]')].map(a => a.getAttribute('href')), text: s.innerText, first: first && first.dataset.sheet };
        });
        check(!!sheet, `${where} ${app}: tapping the CTA should open the in-app sheet`);
        if (sheet) {
          check(sheet.first === (app === 'imessage' ? 'copy' : 'whatsapp'), `${where} ${app}: sheet leads with ${sheet.first}`);
          check(sheet.links.some(h => /^https:\/\/wa\.me\//.test(h) && decodeURIComponent(h).includes('Hey, help me grow my business')), `${where} ${app}: sheet has no prefilled https WhatsApp link`);
          check(sheet.links.every(h => /^https:/.test(h)), `${where} ${app}: sheet links must all be https: ${sheet.links}`);
          check(!/702\D?350\D?8103/.test(sheet.text), `${where} ${app}: phone number is visible in the sheet`);
        }
        await p.click('.send-sheet-x');
      }
      check(navs.length === 0, `${where} tapping navigated away: ${navs}`);
      // If the escape worked (page hidden), no sheet appears behind it.
      await p.evaluate(() => {
        document.querySelector('.send-sheet').hidden = true;
        Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
      });
      await p.click('.send a.send-go');
      await p.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
      await p.waitForTimeout(300);
      check(await p.evaluate(() => document.querySelector('.send-sheet').hidden), `${where} sheet opened although the page left`);
    }
    await ctx.close();
  }
  // Landing in the real browser from an in-app escape (?go=<app>).
  for (const v of VARIANTS) {
    for (const [ua, app, firesSms] of [['safari', 'imessage', true], ['safari', 'whatsapp', false], ['chromeAndroid', 'imessage', true]]) {
      const { ctx, p } = await page(browser, base, v, 390, UA[ua], `?go=${app}&src=tiktok-test`);
      const where = `[h/${v} ${ua} ?go=${app}]`;
      await p.waitForTimeout(500);
      const r = await p.evaluate(() => ({ search: location.search, app: document.querySelector('.send a.send-go').dataset.app,
        href: document.querySelector('.send a.send-go').getAttribute('href'), navs: window.__navs.slice(), n: document.querySelectorAll('.send a.send-go').length }));
      check(!/[?&]go=/.test(r.search) && /src=tiktok-test/.test(r.search), `${where} go= should be stripped and src kept: ${r.search}`);
      check(r.app === app, `${where} picked ${r.app}`);
      check(r.n === 1, `${where} expected one CTA button, found ${r.n}`);
      check(firesSms ? r.navs.length === 1 && /^sms:/.test(r.navs[0]) : r.navs.length === 0, `${where} auto-open: ${r.navs}`);
      await ctx.close();
    }
  }
  // "Text me" in the sheet: on by default, ?textme=0 hides it.
  for (const [query, on] of [['', true], ['?textme=0', false]]) {
    const { ctx, p } = await page(browser, base, 'b', 390, UA.tiktokIos, query);
    const where = `[h/b tiktokIos textme${query || ' off'}]`;
    let posted = null;
    await ctx.route('**/api/textme', r => { posted = r.request().postDataJSON(); r.fulfill({ status: 200, contentType: 'application/json', body: '{"ok":true}' }); });
    await p.click('.send a.send-go');
    await p.waitForTimeout(300);
    const has = await p.evaluate(() => !!document.querySelector('.send-sheet .send-textme'));
    check(has === on, `${where} Text me form shown: ${has}`);
    if (on) {
      await p.fill('.send-textme input[type=tel]', '415');
      await p.click('.send-textme button');
      check(/area code/.test(await p.textContent('.send-textme-msg')), `${where} short number should be rejected`);
      await p.fill('.send-textme input[type=tel]', '(415) 555-0134');
      await p.click('.send-textme button');
      await p.waitForSelector('.send-textme-ok', { timeout: 2000 }).catch(() => {});
      check(posted && posted.phone === '(415) 555-0134' && posted.inapp === 'TikTok', `${where} posted ${JSON.stringify(posted)}`);
      check(!!(await p.$('.send-textme-ok')), `${where} no confirmation after submit`);
    }
    await ctx.close();
  }
}

const DESC = 'Standard Grow, by Standard Trade, is a number you text a reel to. It sends back ads and a budget for your business, runs them once you approve, and reports what they brought in.';
async function aeo(browser, base) {
  for (const v of VARIANTS) {
    const { ctx, p } = await page(browser, base, v, 390);
    const where = `[h/${v} aeo]`;
    const info = await p.evaluate(() => ({
      desc: document.querySelector('meta[name=description]')?.content,
      canonical: document.querySelector('link[rel=canonical]')?.href,
      ld: [...document.querySelectorAll('script[type="application/ld+json"]')].map(s => s.textContent),
      faq: [...document.querySelectorAll('section.faq .faq-i')].map(d => ({ q: d.querySelector('h3').innerText.trim(), a: d.querySelector('p').innerText.trim() })),
    }));
    check(info.desc === DESC, `${where} meta description is "${info.desc}"`);
    check(info.canonical === 'https://grow.standardtrade.ai/', `${where} canonical is ${info.canonical}`);
    check(info.ld.length > 0, `${where} no JSON-LD`);
    let graph = [];
    for (const t of info.ld) { try { const j = JSON.parse(t); graph.push(...(j['@graph'] || [j])); } catch (e) { check(false, `${where} JSON-LD does not parse: ${e.message}`); } }
    const type = t => graph.find(n => n['@type'] === t);
    check(type('Organization')?.name === 'Standard Trade', `${where} JSON-LD has no Standard Trade Organization`);
    check(type('SoftwareApplication')?.name === 'Standard Grow', `${where} JSON-LD has no Standard Grow SoftwareApplication`);
    const ldFaq = (type('FAQPage')?.mainEntity || []).map(q => ({ q: q.name, a: q.acceptedAnswer?.text }));
    check(JSON.stringify(ldFaq) === JSON.stringify(info.faq), `${where} FAQPage JSON-LD does not match the visible FAQ`);
    check(info.faq[0]?.a === DESC, `${where} first FAQ answer is not the canonical description`);
    await ctx.close();
  }
  const key = fs.readdirSync(ROOT).find(f => /^[0-9a-f]{32}\.txt$/.test(f));
  check(!!key, '[aeo] no IndexNow key file in public/');
  const files = [['robots.txt', /Sitemap: https:\/\/grow\.standardtrade\.ai\/sitemap\.xml/], ['sitemap.xml', /<loc>https:\/\/grow\.standardtrade\.ai\/<\/loc>/]];
  if (key) files.push([key, new RegExp('^' + key.slice(0, 32) + '$')]);
  for (const [f, want] of files) {
    const r = await fetch(`${base}/${f}`);
    const t = await r.text();
    check(r.status === 200 && want.test(t), `[aeo] /${f} returned ${r.status} or unexpected content`);
  }
  const robots = fs.readFileSync(path.join(ROOT, 'robots.txt'), 'utf8');
  for (const bot of ['OAI-SearchBot', 'ChatGPT-User', 'GPTBot', 'Bingbot', 'Googlebot']) check(new RegExp(`User-agent: ${bot}\\nAllow: /\\n`).test(robots), `[aeo] robots.txt does not allow ${bot}`);
  const sitemap = fs.readFileSync(path.join(ROOT, 'sitemap.xml'), 'utf8');
  check(!/\/admin|\/tt\//.test(sitemap), '[aeo] sitemap exposes /admin or /tt/');
}

// 4. Public experiment library and growth ledger (built by
//    relay/library/build.py), checked against the AEO page rules: question
//    title <= 60 chars, description <= 160, at least 5 facts each with a
//    source link and checked date, visible FAQ matching FAQPage JSON-LD,
//    BreadcrumbList, a report-a-mistake link, listed in sitemap.xml and
//    llms.txt, and it fits at phone widths.
const LIBRARY = ['/experiments', '/experiments/faceless-tiktok-slideshows', '/ledger', '/what-is-standard-grow', '/text-a-reel'];
async function library(browser, base) {
  const sitemap = fs.readFileSync(path.join(ROOT, 'sitemap.xml'), 'utf8');
  const llms = fs.readFileSync(path.join(ROOT, 'llms.txt'), 'utf8');
  for (const u of LIBRARY) {
    check(sitemap.includes(`<loc>https://grow.standardtrade.ai${u}</loc>`), `[library ${u}] missing from sitemap.xml`);
    check(llms.includes(`(https://grow.standardtrade.ai${u})`), `[library ${u}] missing from llms.txt`);
    for (const width of [320, 430]) {
      const ctx = await browser.newContext({ viewport: { width, height: 844 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, userAgent: UA.safari });
      await ctx.route('**/*', r => r.request().url().startsWith(base) ? r.continue() : r.abort());
      const p = await ctx.newPage();
      const res = await p.goto(`${base}${u}?utm_source=chatgpt.com`, { waitUntil: 'load' });
      const where = `[library ${u} @${width}]`;
      check(res && res.status() === 200, `${where} returned ${res && res.status()}`);
      const info = await p.evaluate(() => ({
        sw: document.documentElement.scrollWidth,
        title: document.title,
        h1: document.querySelector('h1')?.innerText.trim(),
        desc: document.querySelector('meta[name=description]')?.content || '',
        canonical: document.querySelector('link[rel=canonical]')?.href,
        ld: [...document.querySelectorAll('script[type="application/ld+json"]')].map(s => s.textContent),
        facts: [...document.querySelectorAll('li.fact')].map(li => ({ src: li.querySelector('a.src')?.href || '', ck: /checked \d/.test(li.innerText) })),
        faq: [...document.querySelectorAll('section.faq .faq-i')].map(d => ({ q: d.querySelector('h3').innerText.trim(), a: d.querySelector('p').innerText.trim() })),
        mistake: !!document.querySelector('a[href^="mailto:"][href*="Mistake"]'),
        cta: document.querySelector('a.go')?.getAttribute('href'),
      }));
      check(info.sw <= width + TOL, `${where} page is ${info.sw}px wide (horizontal scroll)`);
      check(info.title.length <= 60, `${where} title is ${info.title.length} chars`);
      check(info.h1 === info.title, `${where} h1 should equal the title`);
      check(info.desc.length >= 50 && info.desc.length <= 160, `${where} description is ${info.desc.length} chars`);
      check(info.canonical === `https://grow.standardtrade.ai${u}`, `${where} canonical is ${info.canonical}`);
      check(info.facts.length >= 5 && info.facts.every(f => /^https?:/.test(f.src) && f.ck), `${where} needs >= 5 facts, each with a source link and checked date`);
      check(info.mistake, `${where} no report-a-mistake link`);
      check(info.cta === '/?src=chatgpt.com', `${where} CTA should carry the AI source, got ${info.cta}`);
      let graph = [];
      for (const t of info.ld) { try { const j = JSON.parse(t); graph.push(...(j['@graph'] || [j])); } catch (e) { check(false, `${where} JSON-LD does not parse: ${e.message}`); } }
      const type = t => graph.find(n => n['@type'] === t);
      check(!!type('BreadcrumbList'), `${where} no BreadcrumbList`);
      check(!!type('Article'), `${where} no Article`);
      const ldFaq = (type('FAQPage')?.mainEntity || []).map(q => ({ q: q.name, a: q.acceptedAnswer?.text }));
      check(info.faq.length > 0 && JSON.stringify(ldFaq) === JSON.stringify(info.faq), `${where} FAQPage JSON-LD does not match the visible FAQ`);
      await ctx.close();
    }
  }
  // Referral links carry a valid code into the prefilled text, in Safari,
  // Android and the in-app sheet; a malformed one is dropped.
  for (const [ua, query, want] of [
    ['safari', '?ref=RF_0123456789ab', 'Hey, help me grow my business (ref rf_0123456789ab)'],
    ['chromeAndroid', '?ref=rf_0123456789ab', 'Hey, help me grow my business (ref rf_0123456789ab)'],
    ['safari', '?ref=rf_0123%20x', 'Hey, help me grow my business'],
  ]) {
    const { ctx, p } = await page(browser, base, 'a', 390, UA[ua], query);
    const href = decodeURIComponent(await p.getAttribute('.send a.send-go', 'href') || '');
    check(href.endsWith(want), `[referral ${ua} ${query}] CTA href ${href} should end with "${want}"`);
    await ctx.close();
  }
  {
    const { ctx, p } = await page(browser, base, 'a', 390, UA.tiktokIos, '?ref=rf_0123456789ab');
    await p.tap('.send a.send-go');
    await p.waitForSelector('.send-sheet:not([hidden])', { timeout: 3000 }).catch(() => {});
    const links = await p.$$eval('.send-sheet a', as => as.map(a => decodeURIComponent(a.href)));
    check(links.some(h => h.includes('wa.me') && h.includes('(ref rf_0123456789ab)')), `[referral tiktok] sheet WhatsApp link lacks the code: ${links}`);
    check(links.some(h => h.includes('ref=rf_0123456789ab')) || links.some(h => h.includes('(ref rf_0123456789ab)')), `[referral tiktok] sheet links drop the code: ${links}`);
    await ctx.close();
  }
  // AI attribution: ChatGPT's utm_source reaches ph.js on the hero.
  const { ctx, p } = await page(browser, base, 'a', 390);
  await p.goto(`${base}/h/a?utm_source=chatgpt.com`, { waitUntil: 'load' });
  check(await p.evaluate(() => window.sgAiSrc) === 'chatgpt', '[ai attribution] utm_source=chatgpt.com should set ai_src chatgpt');
  await ctx.close();
}

const server = await serve();
const base = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch();
try {
  await layout(browser, base);
  await cta(browser, base);
  await aeo(browser, base);
  await library(browser, base);
} finally {
  await browser.close();
  server.close();
}
if (failures.length) {
  console.error(`FAIL landing: ${failures.length} of ${checks} checks failed`);
  failures.forEach(f => console.error('  ✗ ' + f));
  process.exit(1);
}
console.log(`PASS landing: ${checks} checks (variants ${VARIANTS.join('/')}, widths ${WIDTHS.join('/')}, CTA x6 user agents, FAQ/meta/JSON-LD, robots/sitemap/IndexNow, experiment library)`);
