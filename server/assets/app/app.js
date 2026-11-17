// DropCraft Web-App: Dateien, Freigaben und Konto im Browser.
// Für iPhone & Co.: in Safari öffnen → Teilen → „Zum Home-Bildschirm“.
'use strict';

(function () {
  const $app = document.getElementById('app');
  const $sheet = document.getElementById('sheet-root');
  const $toast = document.getElementById('toast');

  // ------------------------------------------------------------ Hilfen

  const esc = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const join = (dir, name) => (dir ? dir + '/' + name : name);
  const parent = (p) => (p.includes('/') ? p.slice(0, p.lastIndexOf('/')) : '');
  const baseName = (p) => p.split('/').pop();
  const ext = (n) => (n.includes('.') ? n.split('.').pop().toLowerCase() : '');

  function size(b) {
    if (b < 1024) return b + ' B';
    const u = ['KB', 'MB', 'GB', 'TB'];
    let v = b / 1024, i = 0;
    while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
    return v.toLocaleString('de-DE', { maximumFractionDigits: v < 10 ? 1 : 0 }) + ' ' + u[i];
  }

  function date(ms) {
    const d = new Date(ms), now = new Date();
    if (d.toDateString() === now.toDateString()) return d.toLocaleTimeString('de-DE', { hour: '2-digit', minute: '2-digit' });
    return d.toLocaleDateString('de-DE', { day: '2-digit', month: '2-digit', year: 'numeric' });
  }

  function expiry(ms) {
    const left = ms - Date.now();
    if (left <= 0) return 'abgelaufen';
    const h = Math.floor(left / 3600000);
    const rest = h < 1 ? 'unter 1 Stunde' : h === 1 ? 'noch 1 Stunde' : h < 48 ? `noch ${h} Stunden` : `noch ${Math.floor(h / 24)} Tage`;
    const d = new Date(ms);
    return `bis ${d.toLocaleDateString('de-DE')}, ${d.toLocaleTimeString('de-DE', { hour: '2-digit', minute: '2-digit' })} · ${rest}`;
  }

  let toastTimer;
  function toast(msg) {
    $toast.textContent = msg;
    $toast.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => ($toast.hidden = true), 3000);
  }

  // Symbole (Linien, 16er-Raster)
  const P = {
    folder: '<path d="M1.8 3.5h4.2l1.5 1.7h6.7v7.6H1.8z"/>',
    file: '<path d="M3.5 1.5h6l3 3v10h-9z"/><path d="M9.5 1.5v3h3"/>',
    image: '<rect x="2" y="3" width="12" height="10" rx="1"/><circle cx="6" cy="6.5" r="1.1"/><path d="M2.5 12l3.5-3.5 2.5 2.5 2-2 3 3"/>',
    media: '<rect x="2" y="3" width="12" height="10" rx="1"/><path d="M6.5 6l4 2-4 2z"/>',
    audio: '<path d="M2 8h1.5M4.5 5.5v5M7 3v10M9.5 5v6M12 6.5v3M14 8h.5"/>',
    link: '<path d="M6.3 9.7l3.4-3.4"/><path d="M7.5 4.6l1.4-1.4a2.6 2.6 0 0 1 3.9 3.7l-1.4 1.5M8.5 11.4l-1.4 1.4a2.6 2.6 0 0 1-3.9-3.7l1.4-1.5"/>',
    user: '<circle cx="8" cy="5.5" r="2.8"/><path d="M2.8 13.8a5.2 5.2 0 0 1 10.4 0"/>',
    plus: '<path d="M8 3v10M3 8h10"/>',
    more: '<circle cx="3.5" cy="8" r=".9"/><circle cx="8" cy="8" r=".9"/><circle cx="12.5" cy="8" r=".9"/>',
    back: '<path d="M10 3L5 8l5 5"/>',
    chev: '<path d="M6 4l4 4-4 4"/>',
    up: '<path d="M8 13V3M4 7l4-4 4 4"/>',
    down: '<path d="M8 3v10M4 9l4 4 4-4"/>',
    upload: '<path d="M8 10.5v-8M5 5.5l3-3 3 3"/><path d="M2.5 10.5v3h11v-3"/>',
    lock: '<rect x="3.5" y="7" width="9" height="6.5" rx="1"/><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"/>',
    clock: '<circle cx="8" cy="8" r="6"/><path d="M8 4.5V8l2.5 1.5"/>',
    bell: '<path d="M4 11.5V7a4 4 0 0 1 8 0v4.5l1 1H3z"/><path d="M6.8 14.2h2.4"/>',
    close: '<path d="M4 4l8 8M12 4l-8 8"/>',
    share: '<path d="M8 2v8M5 5l3-3 3 3"/><path d="M4 7.5H3v6.5h10V7.5h-1"/>',
    users: '<circle cx="6" cy="5.5" r="2.4"/><path d="M1.8 13.5a4.2 4.2 0 0 1 8.4 0"/><path d="M10.5 3.4a2.4 2.4 0 0 1 0 4.4M12 9.6a4.2 4.2 0 0 1 2.2 3.9"/>',
    logout: '<path d="M6.5 2.5h-4v11h4M10 5l3 3-3 3M13 8H6"/>',
    cloud: '<path d="M4.5 12.5a3 3 0 0 1-.4-6A4 4 0 0 1 11.8 6a3.2 3.2 0 0 1 .2 6.5z"/>',
  };
  const ic = (n, s = 18) =>
    `<svg width="${s}" height="${s}" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${P[n]}</svg>`;

  function fileIcon(name, isDir) {
    if (isDir) return 'folder';
    const e = ext(name);
    if (['jpg', 'jpeg', 'png', 'gif', 'webp', 'heic', 'bmp'].includes(e)) return 'image';
    if (['mp4', 'mov', 'm4v', 'webm', 'mkv', 'avi'].includes(e)) return 'media';
    if (['mp3', 'wav', 'm4a', 'aac', 'flac', 'ogg'].includes(e)) return 'audio';
    return 'file';
  }
  const kind = (name) => {
    const e = ext(name);
    if (['jpg', 'jpeg', 'png', 'gif', 'webp', 'heic', 'bmp', 'svg'].includes(e)) return 'image';
    if (['mp4', 'mov', 'm4v', 'webm'].includes(e)) return 'video';
    if (['mp3', 'wav', 'm4a', 'aac', 'flac', 'ogg'].includes(e)) return 'audio';
    return 'other';
  };

  // ------------------------------------------------------------ API

  const store = {
    get token() { try { return localStorage.getItem('dc.token'); } catch { return null; } },
    set token(v) { try { v ? localStorage.setItem('dc.token', v) : localStorage.removeItem('dc.token'); } catch {} },
  };

  class ApiError extends Error {
    constructor(status, message) { super(message); this.status = status; }
  }

  async function api(method, path, { query, json } = {}) {
    let url = '/api' + path;
    if (query) url += '?' + new URLSearchParams(query).toString();
    const headers = {};
    if (store.token) headers.Authorization = 'Bearer ' + store.token;
    if (json !== undefined) headers['Content-Type'] = 'application/json';
    let r;
    try {
      r = await fetch(url, { method, headers, body: json !== undefined ? JSON.stringify(json) : undefined });
    } catch {
      throw new ApiError(0, 'Keine Verbindung zum Server.');
    }
    if (r.status === 401 && path !== '/auth/login') {
      store.token = null;
      state.me = null;
      route();
      throw new ApiError(401, 'Bitte neu anmelden.');
    }
    if (!r.ok) {
      let msg = '';
      try { msg = (await r.json()).message || ''; } catch {}
      if (r.status === 507) msg = msg || 'Kein Speicherplatz mehr frei.';
      if (r.status === 401) msg = msg || 'Benutzername oder Passwort falsch.';
      throw new ApiError(r.status, msg || `Fehler (${r.status})`);
    }
    const ct = r.headers.get('content-type') || '';
    return ct.includes('json') ? r.json() : null;
  }

  const fail = (e) => toast(e.message || String(e));

  // ------------------------------------------------------------ Zustand & Routen

  const state = { me: null, transfers: [], shares: null };

  function go(hash) {
    if (location.hash === hash) route();
    else location.hash = hash;
  }

  window.addEventListener('hashchange', route);

  async function route() {
    closeSheet();
    if (!store.token) return renderLogin();
    if (!state.me) {
      try { state.me = await api('GET', '/me'); } catch (e) { if (!store.token) return; }
    }
    const h = decodeURIComponent(location.hash.slice(1));
    if (h.startsWith('/shares/')) return renderShare(h.slice(8));
    if (h === '/shares') return renderShares();
    if (h === '/account') return renderAccount();
    const dir = h.startsWith('/files') ? h.slice(7) : '';
    return renderFolder(dir);
  }

  function shell(title, { left = '', right = '', tab = 'files', body = '' }) {
    $app.innerHTML = `<div class="shell">
<header class="bar"><div class="side">${left}</div><h1>${esc(title)}</h1><div class="side right">${right}</div></header>
<main class="main" id="main">${body}</main>
<nav class="tabs">
<a href="#/files" class="${tab === 'files' ? 'on' : ''}">${ic('folder', 22)}Dateien</a>
<a href="#/shares" class="${tab === 'shares' ? 'on' : ''}">${ic('link', 22)}Freigaben</a>
<a href="#/account" class="${tab === 'account' ? 'on' : ''}">${ic('user', 22)}Konto</a>
</nav></div>`;
    return document.getElementById('main');
  }

  // ------------------------------------------------------------ Anmeldung

  function renderLogin() {
    $app.innerHTML = `<form class="login" id="login">
<img class="logo" src="/app/icon-180.png" alt="">
<h1>DropCraft</h1>
<div class="group">
<div class="field"><label for="u">Benutzername</label><input class="input" id="u" autocomplete="username" autocapitalize="none" autocorrect="off" required></div>
<div class="field"><label for="p">Passwort</label><input class="input" id="p" type="password" autocomplete="current-password" required></div>
</div>
<div class="error" id="err" hidden></div>
<button class="btn primary block" id="go">Anmelden</button>
<p class="hint">Tipp: In Safari über Teilen → „Zum Home-Bildschirm“ wird DropCraft zur App.</p>
</form>`;
    const form = document.getElementById('login');
    form.addEventListener('submit', async (ev) => {
      ev.preventDefault();
      const btn = document.getElementById('go'), err = document.getElementById('err');
      btn.disabled = true;
      err.hidden = true;
      try {
        const ua = navigator.userAgent;
        const dev = /iPhone/.test(ua) ? 'iPhone' : /iPad|Macintosh.*Mobile/.test(ua) ? 'iPad' : /Android/.test(ua) ? 'Android' : 'Browser';
        const r = await api('POST', '/auth/login', {
          json: { username: document.getElementById('u').value.trim(), password: document.getElementById('p').value, device_name: dev + ' (Web)' },
        });
        store.token = r.token;
        state.me = null;
        go('#/files');
      } catch (e) {
        err.textContent = e.message;
        err.hidden = false;
        btn.disabled = false;
      }
    });
  }

  // ------------------------------------------------------------ Dateien

  let currentDir = null;
  let currentItems = [];

  function transfersHtml() {
    if (!state.transfers.length) return '';
    return `<div class="group" id="xfers">${state.transfers
      .map((t) => `<div class="xfer" data-id="${t.id}">${ic(t.up ? 'up' : 'down')}<div class="txt"><div class="top"><span>${esc(t.name)}</span><span class="pct">${Math.round(t.p * 100)} %</span></div><div class="track"><div class="fill"></div></div></div></div>`)
      .join('')}</div><div class="label"></div>`;
  }

  function paintTransfers() {
    for (const t of state.transfers) {
      const el = document.querySelector(`.xfer[data-id="${t.id}"]`);
      if (!el) continue;
      el.querySelector('.fill').style.width = Math.round(t.p * 100) + '%';
      el.querySelector('.pct').textContent = Math.round(t.p * 100) + ' %';
    }
  }

  async function renderFolder(dir) {
    currentDir = dir;
    const title = dir ? baseName(dir) : 'DropCraft';
    const left = dir ? `<a class="iconbtn text" href="#/files/${encodeURIComponent(parent(dir))}" aria-label="Zurück">${ic('back', 20)}</a>` : '';
    const right = `<button class="iconbtn" id="add" aria-label="Hinzufügen">${ic('plus', 22)}</button>`;
    const main = shell(title, { left, right, tab: 'files', body: transfersHtml() + '<div class="spinner"></div>' });
    paintTransfers();
    document.getElementById('add').onclick = () => addMenu(dir);
    let items;
    try {
      items = (await api('GET', '/files/list', { query: { path: dir } })).entries;
    } catch (e) {
      if (currentDir !== dir) return;
      main.innerHTML = transfersHtml() + `<div class="empty">${esc(e.message)}</div>`;
      return;
    }
    if (currentDir !== dir) return;
    currentItems = items;
    const rows = items
      .map((it, i) => {
        const meta = it.is_dir ? date(it.modified_at) : `${size(it.size)} · ${date(it.modified_at)}`;
        const icon = `<span class="ic ${it.is_dir ? 'folder' : ''}">${ic(fileIcon(it.name, it.is_dir))}</span>`;
        return `<div class="item">
<button class="row open" data-i="${i}">${icon}<span class="txt"><span class="name">${esc(it.name)}</span><span class="meta">${esc(meta)}</span></span></button>
<button class="iconbtn more" data-i="${i}" aria-label="Aktionen">${ic('more', 20)}</button></div>`;
      })
      .join('');
    const files = items.filter((i) => !i.is_dir);
    main.innerHTML =
      transfersHtml() +
      (items.length
        ? `<div class="group list">${rows}</div><div class="foot">${items.length} Einträge · ${size(files.reduce((a, b) => a + b.size, 0))}</div>`
        : `<div class="empty">${ic('folder', 48)}Leerer Ordner.<br>Mit + Fotos oder Dateien hochladen.</div>`);
    paintTransfers();
    main.querySelectorAll('button.open').forEach((b) => (b.onclick = () => openItem(items[+b.dataset.i])));
    main.querySelectorAll('button.more').forEach((b) => (b.onclick = () => itemMenu(items[+b.dataset.i])));
    loadThumbs(main, items);
  }

  // Vorschaubilder für Bilder in der Liste (signierte Links)
  async function loadThumbs(main, items) {
    for (let i = 0; i < items.length; i++) {
      const it = items[i];
      if (it.is_dir || kind(it.name) !== 'image' || ext(it.name) === 'heic' || it.size > 15 * 1024 * 1024) continue;
      try {
        const { url } = await api('GET', '/files/link', { query: { path: it.path } });
        const span = main.querySelector(`button.open[data-i="${i}"] .ic`);
        if (!span) return;
        const img = new Image();
        img.alt = '';
        img.src = url;
        img.onload = () => { span.textContent = ''; span.appendChild(img); };
      } catch { return; }
    }
  }

  function openItem(it) {
    if (it.is_dir) return go('#/files/' + encodeURIComponent(it.path));
    viewer(it);
  }

  async function signed(path, download) {
    const { url } = await api('GET', '/files/link', { query: { path } });
    return download ? url + '&download=1' : url;
  }

  async function viewer(it) {
    let url;
    try { url = await signed(it.path); } catch (e) { return fail(e); }
    const k = kind(it.name);
    const stage =
      k === 'image' ? `<img src="${esc(url)}" alt="">`
      : k === 'video' ? `<video src="${esc(url)}" controls playsinline autoplay></video>`
      : k === 'audio' ? `<audio src="${esc(url)}" controls autoplay></audio>`
      : `<div class="other">${ic(fileIcon(it.name, false), 56)}<p>${esc(it.name)}<br>${size(it.size)}</p><a class="btn primary" href="${esc(url)}" target="_blank" rel="noopener">Öffnen</a></div>`;
    const el = document.createElement('div');
    el.className = 'viewer';
    el.innerHTML = `<div class="vbar"><button class="iconbtn" id="vclose" aria-label="Schließen">${ic('close', 20)}</button><h2>${esc(it.name)}</h2><button class="iconbtn" id="vmore" aria-label="Aktionen">${ic('share', 20)}</button></div><div class="stage">${stage}</div>`;
    document.body.appendChild(el);
    const close = () => { el.querySelectorAll('video,audio').forEach((m) => m.pause()); el.remove(); };
    el.querySelector('#vclose').onclick = close;
    el.querySelector('#vmore').onclick = () => itemMenu(it, close);
  }

  // ------------------------------------------------------------ Blätter

  function closeSheet() { $sheet.innerHTML = ''; }

  /// Aktionsblatt: [{label, danger, run}]
  function actionSheet(title, actions) {
    $sheet.innerHTML = `<div class="backdrop"><div class="sheet"><div class="group">${title ? `<div class="title">${esc(title)}</div>` : ''}${actions
      .map((a, i) => `<button class="row ${a.danger ? 'danger' : ''}" data-i="${i}">${esc(a.label)}</button>`)
      .join('')}</div><div class="group"><button class="row cancel">Abbrechen</button></div></div></div>`;
    const bd = $sheet.querySelector('.backdrop');
    bd.onclick = (e) => { if (e.target === bd) closeSheet(); };
    $sheet.querySelector('.cancel').onclick = closeSheet;
    $sheet.querySelectorAll('[data-i]').forEach((b) => (b.onclick = () => { closeSheet(); actions[+b.dataset.i].run(); }));
  }

  /// Dialog mit Text oder Eingabefeld. Gibt Promise<string|true|null>.
  function dialog({ title, text = '', input = null, placeholder = '', ok = 'OK', danger = false, type = 'text' }) {
    return new Promise((resolve) => {
      $sheet.innerHTML = `<div class="backdrop center"><form class="dialog"><div class="dbody"><h3>${esc(title)}</h3>${text ? `<p>${esc(text)}</p>` : ''}${
        input !== null ? `<input class="input" type="${type}" id="dlg-in" placeholder="${esc(placeholder)}" autocapitalize="none" autocorrect="off">` : ''
      }</div><div class="dbtns"><button type="button" id="dlg-no">Abbrechen</button><button type="submit" class="${danger ? 'danger' : ''}">${esc(ok)}</button></div></form></div>`;
      const bd = $sheet.querySelector('.backdrop');
      bd.style.alignItems = 'center';
      const inp = $sheet.querySelector('#dlg-in');
      if (inp) { inp.value = input; setTimeout(() => { inp.focus(); inp.select(); }, 50); }
      const done = (v) => { closeSheet(); resolve(v); };
      $sheet.querySelector('#dlg-no').onclick = () => done(null);
      $sheet.querySelector('form').onsubmit = (e) => { e.preventDefault(); done(inp ? inp.value.trim() : true); };
    });
  }

  // ------------------------------------------------------------ Aktionen

  function addMenu(dir) {
    actionSheet(null, [
      { label: 'Fotos oder Dateien hochladen', run: () => pickFiles(dir) },
      { label: 'Neuer Ordner', run: () => newFolder(dir) },
      ...(dir ? [{ label: 'Diesen Ordner freigeben', run: () => shareLink(dir) }] : []),
    ]);
  }

  function itemMenu(it, closeViewer) {
    const acts = [{ label: 'Link kopieren', run: () => shareLink(it.path) }];
    if (navigator.share) acts.push({ label: 'Link senden …', run: () => shareLink(it.path, true) });
    if (!it.is_dir) acts.push({ label: 'Herunterladen', run: () => download(it) });
    acts.push({ label: 'Umbenennen', run: () => rename(it) });
    acts.push({ label: 'Löschen', danger: true, run: () => remove(it, closeViewer) });
    actionSheet(it.name, acts);
  }

  async function shareLink(path, send) {
    try {
      const list = await api('GET', '/shares');
      const sh = list.find((s) => s.path === path) || (await api('POST', '/shares', { json: { path } }));
      const url = location.origin + '/s/' + sh.token;
      if (send && navigator.share) {
        try { await navigator.share({ title: baseName(path), url }); } catch {}
        return;
      }
      try {
        await navigator.clipboard.writeText(url);
        toast(`Link für „${baseName(path)}“ kopiert`);
      } catch {
        await dialog({ title: 'Link', text: url, ok: 'Fertig' });
      }
    } catch (e) { fail(e); }
  }

  async function download(it) {
    try {
      const url = await signed(it.path, true);
      const a = document.createElement('a');
      a.href = url;
      a.download = it.name;
      document.body.appendChild(a);
      a.click();
      a.remove();
    } catch (e) { fail(e); }
  }

  async function newFolder(dir) {
    const name = await dialog({ title: 'Neuer Ordner', input: '', placeholder: 'Name', ok: 'Anlegen' });
    if (!name) return;
    try { await api('POST', '/files/mkdir', { json: { path: join(dir, name) } }); route(); } catch (e) { fail(e); }
  }

  async function rename(it) {
    const name = await dialog({ title: 'Umbenennen', input: it.name, ok: 'Umbenennen' });
    if (!name || name === it.name) return;
    try { await api('POST', '/files/move', { json: { from: it.path, to: join(parent(it.path), name) } }); route(); } catch (e) { fail(e); }
  }

  async function remove(it, closeViewer) {
    const ok = await dialog({ title: `„${it.name}“ löschen?`, text: 'Wird auf allen Geräten gelöscht.', ok: 'Löschen', danger: true });
    if (!ok) return;
    try {
      await api('POST', '/files/delete', { json: { path: it.path } });
      if (closeViewer) closeViewer();
      route();
    } catch (e) { fail(e); }
  }

  // ------------------------------------------------------------ Hochladen

  function pickFiles(dir) {
    const inp = document.createElement('input');
    inp.type = 'file';
    inp.multiple = true;
    inp.onchange = () => uploadAll(dir, [...inp.files]);
    inp.click();
  }

  function freeName(name, taken) {
    let cand = name, n = 2;
    const dot = name.lastIndexOf('.');
    const base = dot > 0 ? name.slice(0, dot) : name, ex = dot > 0 ? name.slice(dot) : '';
    while (taken.has(cand.toLowerCase())) cand = `${base} (${n++})${ex}`;
    taken.add(cand.toLowerCase());
    return cand;
  }

  let xferId = 0;
  async function uploadAll(dir, files) {
    const taken = new Set((dir === currentDir ? currentItems : []).map((i) => i.name.toLowerCase()));
    const jobs = files.map((f) => ({ id: ++xferId, name: freeName(f.name, taken), up: true, p: 0, file: f }));
    state.transfers.push(...jobs);
    if (currentDir === dir) route();
    // Bis zu drei Dateien gleichzeitig
    const queue = jobs.slice();
    const worker = async () => {
      for (let j = queue.shift(); j; j = queue.shift()) {
        try {
          await uploadOne(dir, j);
        } catch (e) {
          fail(e);
        }
        state.transfers = state.transfers.filter((t) => t.id !== j.id);
        document.querySelector(`.xfer[data-id="${j.id}"]`)?.remove();
      }
    };
    await Promise.all(Array.from({ length: Math.min(3, jobs.length) }, worker));
    if (currentDir === dir) route();
    toast(jobs.length === 1 ? `„${jobs[0].name}“ hochgeladen` : `${jobs.length} Dateien hochgeladen`);
  }

  function uploadOne(dir, job) {
    return new Promise((resolve, reject) => {
      const q = new URLSearchParams({ path: join(dir, job.name), mtime: String(job.file.lastModified || Date.now()) });
      const x = new XMLHttpRequest();
      x.open('PUT', '/api/files/upload?' + q.toString());
      x.setRequestHeader('Authorization', 'Bearer ' + store.token);
      x.setRequestHeader('Content-Type', 'application/octet-stream');
      x.upload.onprogress = (e) => {
        if (e.lengthComputable) { job.p = e.loaded / e.total; paintTransfers(); }
      };
      x.onload = () => {
        if (x.status >= 200 && x.status < 300) return resolve();
        let msg = '';
        try { msg = JSON.parse(x.responseText).message; } catch {}
        reject(new Error(x.status === 507 ? 'Kein Speicherplatz mehr frei.' : msg || `Upload fehlgeschlagen (${x.status})`));
      };
      x.onerror = () => reject(new Error('Verbindung abgebrochen.'));
      x.send(job.file);
    });
  }

  // ------------------------------------------------------------ Freigaben

  function chipsHtml(s) {
    const c = [];
    if (s.allow_upload) c.push(`<span class="chip accent">${ic('upload', 10)}Upload</span>`);
    if (s.has_password) c.push(`<span class="chip">${ic('lock', 10)}Passwort</span>`);
    if (s.expires_at) {
      const left = s.expires_at - Date.now();
      if (left <= 0) c.push(`<span class="chip danger">${ic('clock', 10)}abgelaufen</span>`);
      else if (left < 86400000) c.push(`<span class="chip warn">${ic('clock', 10)}läuft heute ab</span>`);
      else c.push(`<span class="chip">${ic('clock', 10)}noch ${Math.floor(left / 86400000)} Tage</span>`);
    }
    if (s.notify) c.push(`<span class="chip">${ic('bell', 10)}</span>`);
    return `<span class="chips">${c.join('')}</span>`;
  }

  async function renderShares() {
    const main = shell('Freigaben', { tab: 'shares', body: '<div class="spinner"></div>' });
    try { state.shares = await api('GET', '/shares'); } catch (e) { main.innerHTML = `<div class="empty">${esc(e.message)}</div>`; return; }
    const list = state.shares;
    if (!list.length) {
      main.innerHTML = `<div class="empty">${ic('link', 48)}Noch nichts freigegeben.<br>Bei einer Datei oder einem Ordner auf ··· tippen und „Link kopieren“ wählen.</div>`;
      return;
    }
    main.innerHTML = `<div class="group">${list
      .map((s) => {
        const name = baseName(s.path);
        const isDir = !name.includes('.');
        return `<a class="row" href="#/shares/${encodeURIComponent(s.token)}"><span class="ic ${isDir ? 'folder' : ''}">${ic(fileIcon(name, isDir))}</span><span class="txt"><span class="name">${esc(name)}</span>${chipsHtml(s)}</span><span class="chev">${ic('chev', 16)}</span></a>`;
      })
      .join('')}</div><div class="foot">${list.length} ${list.length === 1 ? 'aktiver Link' : 'aktive Links'}</div>`;
  }

  async function renderShare(token) {
    const left = `<a class="iconbtn text" href="#/shares" aria-label="Zurück">${ic('back', 20)}</a>`;
    const main = shell('Freigabe', { left, tab: 'shares', body: '<div class="spinner"></div>' });
    let s;
    try { s = (await api('GET', '/shares')).find((x) => x.token === token); } catch (e) { return fail(e); }
    if (!s) { main.innerHTML = '<div class="empty">Freigabe nicht gefunden.</div>'; return; }
    document.querySelector('.bar h1').textContent = baseName(s.path);
    const url = location.origin + '/s/' + s.token;
    const exp = s.expires_at;
    const days = [['Immer', 0], ['1 Tag', 1], ['7 Tage', 7], ['30 Tage', 30]];
    main.innerHTML = `
<div class="label">Link</div>
<div class="group">
<a class="row" href="${esc(url)}" target="_blank" rel="noopener"><span class="txt"><span class="name accent">${esc(url)}</span><span class="meta">${esc(s.path)}</span></span></a>
<button class="row accent" id="copy">${ic('link')}Link kopieren</button>
${navigator.share ? `<button class="row accent" id="send">${ic('share')}Link senden …</button>` : ''}
</div>
<div class="label">Einstellungen</div>
<div class="group">
<label class="row">${ic('upload')}<span class="txt">Gäste dürfen hochladen</span><span class="switch"><input type="checkbox" id="up" ${s.allow_upload ? 'checked' : ''}><span></span></span></label>
<label class="row">${ic('bell')}<span class="txt">Benachrichtigen</span><span class="switch"><input type="checkbox" id="nt" ${s.notify ? 'checked' : ''}><span></span></span></label>
</div>
<div class="label">Passwort</div>
<div class="group">
<div class="row">${ic('lock')}<span class="txt">${s.has_password ? 'Geschützt' : 'Kein Passwort'}</span>
${s.has_password ? '<button class="iconbtn text" id="pwdel">Entfernen</button>' : ''}<button class="iconbtn text" id="pwset">${s.has_password ? 'Ändern' : 'Festlegen'}</button></div>
</div>
<div class="label">Gültigkeit</div>
<div class="group"><div class="field"><div class="seg" id="seg">${days
      .map(([l, d]) => `<button data-d="${d}" class="${d === 0 && !exp ? 'on' : ''}">${l}</button>`)
      .join('')}</div>${exp ? `<label>${esc(expiry(exp))}</label>` : ''}</div></div>
<div class="label"></div>
<div class="group"><button class="row danger" id="del">Freigabe beenden</button></div>`;
    const update = async (body, msg) => {
      try { await api('PATCH', '/shares/' + token, { json: body }); if (msg) toast(msg); route(); } catch (e) { fail(e); }
    };
    main.querySelector('#copy').onclick = async () => {
      try { await navigator.clipboard.writeText(url); toast('Link kopiert'); } catch { await dialog({ title: 'Link', text: url, ok: 'Fertig' }); }
    };
    const send = main.querySelector('#send');
    if (send) send.onclick = () => navigator.share({ title: baseName(s.path), url }).catch(() => {});
    main.querySelector('#up').onchange = (e) => update({ allow_upload: e.target.checked });
    main.querySelector('#nt').onchange = (e) => update({ notify: e.target.checked });
    main.querySelector('#pwset').onclick = async () => {
      const pw = await dialog({ title: 'Passwort für den Link', input: '', type: 'password', placeholder: 'mind. 4 Zeichen', ok: 'Speichern' });
      if (pw === null) return;
      if (pw.length < 4) return toast('Mindestens 4 Zeichen');
      update({ password: pw }, 'Passwort gesetzt');
    };
    const pwdel = main.querySelector('#pwdel');
    if (pwdel) pwdel.onclick = () => update({ password: '' }, 'Passwort entfernt');
    main.querySelectorAll('#seg button').forEach((b) => {
      b.onclick = () => {
        const d = +b.dataset.d;
        update({ expires_at: d === 0 ? 0 : Date.now() + d * 86400000 });
      };
    });
    main.querySelector('#del').onclick = async () => {
      if (!(await dialog({ title: 'Freigabe beenden?', text: 'Der Link funktioniert danach nicht mehr.', ok: 'Beenden', danger: true }))) return;
      try { await api('DELETE', '/shares/' + token); toast('Freigabe beendet'); go('#/shares'); } catch (e) { fail(e); }
    };
  }

  // ------------------------------------------------------------ Konto

  async function renderAccount() {
    const main = shell('Konto', { tab: 'account', body: '<div class="spinner"></div>' });
    try { state.me = await api('GET', '/me'); } catch (e) { return fail(e); }
    const me = state.me;
    const frac = me.quota_bytes > 0 ? Math.min(1, me.used_bytes / me.quota_bytes) : 0;
    const standalone = window.matchMedia('(display-mode: standalone)').matches || navigator.standalone;
    const ios = /iPhone|iPad|iPod/.test(navigator.userAgent);
    main.innerHTML = `
${!standalone && ios ? `<div class="install">${ic('share', 18)}<span>Als App nutzen: in Safari auf Teilen tippen und „Zum Home-Bildschirm“ wählen.</span></div>` : ''}
<div class="group quota"><div class="head"><span class="big">${size(me.used_bytes)} belegt</span><span class="meta">von ${size(me.quota_bytes)}</span></div>
<div class="track"><div class="fill" id="qf"></div></div></div>
<div class="label">Angemeldet</div>
<div class="group"><div class="row"><span class="ic">${ic('user')}</span><span class="txt"><span class="name">${esc(me.username)}${me.is_admin ? ' · Admin' : ''}</span><span class="meta">${esc(location.host)}</span></span></div></div>
<div class="label"></div>
<div class="group"><button class="row" id="manage"><span class="ic">${ic(me.is_admin ? 'users' : 'user')}</span><span class="txt"><span class="name">${me.is_admin ? 'Konto & Verwaltung' : 'Konto verwalten'}</span><span class="meta">Passwort, Geräte, Links${me.is_admin ? ', Benutzer, Einstellungen' : ''}</span></span><span class="chev">${ic('chev', 16)}</span></button></div>
<div class="label"></div>
<div class="group"><button class="row danger" id="logout">${ic('logout')}Abmelden</button></div>`;
    const qf = main.querySelector('#qf');
    qf.style.width = Math.round(frac * 100) + '%';
    if (frac > 0.95) qf.classList.add('full');
    // Konto-Website auf dem Server, gleich angemeldet (Einmal-Code).
    main.querySelector('#manage').onclick = async () => {
      try {
        const r = await api('POST', '/account/web-login', { json: {} });
        location.href = r.url;
      } catch (e) { location.href = '/account'; }
    };
    main.querySelector('#logout').onclick = async () => {
      if (!(await dialog({ title: 'Abmelden?', ok: 'Abmelden', danger: true }))) return;
      try { await api('POST', '/auth/logout'); } catch {}
      store.token = null;
      state.me = null;
      go('#/files');
    };
  }

  // ------------------------------------------------------------ Start

  if ('serviceWorker' in navigator) navigator.serviceWorker.register('/app/sw.js', { scope: '/app/' }).catch(() => {});
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible' && store.token && !document.querySelector('.viewer') && !$sheet.innerHTML && !state.transfers.length) route();
  });
  route();
})();
