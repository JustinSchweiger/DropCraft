'use strict';
// Musik-Check: Oberfläche, Wiedergabe und Auswertung

const $ = (s) => document.querySelector(s);
const FPS = 100; // Frames pro Sekunde (10 ms)
const FARBE = { ok: '#34d399', warn: '#fbbf24', bad: '#ef4455', leer: '#55606c' };
const STREIFEN = { ok: '#123a2e', warn: '#3d300c', bad: '#43161c', leer: '#1a2028' };

const S = {
  view: { t0: 0, t1: 1 }, sel: -1, mode: 'orig', playing: false, offset: 0,
  grids: new Map(), dirty: true, hoverCut: -1, trim: [0, 0],
};
const speicher = {
  get(k, d) { try { const v = localStorage.getItem('musikcheck.' + k); return v == null ? d : v; } catch (e) { return d; } },
  set(k, v) { try { localStorage.setItem('musikcheck.' + k, v); } catch (e) { /* egal */ } },
};

// ---------- Ansicht: einfach / Profi ----------
function ansicht(v) {
  S.ansicht = v;
  document.body.classList.toggle('einfach', v === 'einfach');
  document.body.classList.toggle('profi', v === 'profi');
  $('#v-einfach').classList.toggle('on', v === 'einfach');
  $('#v-profi').classList.toggle('on', v === 'profi');
  speicher.set('ansicht', v);
  if (v === 'einfach' && S.dur) setView(0, S.dur);
  if (S.segs) zeichneAlles();
}
ansicht(speicher.get('ansicht', 'einfach'));
$('#v-einfach').addEventListener('click', () => ansicht('einfach'));
$('#v-profi').addEventListener('click', () => ansicht('profi'));
// Für den Prüfbericht immer die ausführliche Ansicht drucken
let vorDruck = null;
window.addEventListener('beforeprint', () => { if (S.ansicht !== 'profi') { vorDruck = S.ansicht; ansicht('profi'); } });
window.addEventListener('afterprint', () => { if (vorDruck) { ansicht(vorDruck); vorDruck = null; } });

// ---------- Zahlen und Zeiten ----------
const minus = (s) => s.replace('-', '−');
function zahl(x, d = 1) {
  if (!isFinite(x)) return '–';
  const f = Math.pow(10, d), r = Math.round(x * f) / f;
  return minus((r === 0 ? 0 : r).toFixed(d).replace('.', ','));
}
function plus(x, d = 1) {
  if (!isFinite(x)) return '–';
  const s = zahl(x, d);
  return Math.round(x * Math.pow(10, d)) > 0 ? '+' + s : s;
}
function zeit(t, zehntel = false) {
  t = Math.max(0, t);
  const m = Math.floor(t / 60), s = t - m * 60;
  if (zehntel) return m + ':' + (s < 10 ? '0' : '') + s.toFixed(1).replace('.', ',');
  const si = Math.floor(s);
  return m + ':' + (si < 10 ? '0' : '') + si;
}
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));
function toast(text, ms = 3200) {
  const t = $('#toast');
  t.textContent = text; t.classList.add('zeigen');
  clearTimeout(toast.h); toast.h = setTimeout(() => t.classList.remove('zeigen'), ms);
}

// ---------- Worker ----------
const worker = new Worker('worker.js');
let wartend = {};
let fortschritt = () => {};
worker.onmessage = (e) => {
  const m = e.data;
  if (m.type === 'progress') return fortschritt(m.p);
  if (m.type === 'error') {
    for (const k in wartend) wartend[k].rej(new Error(m.message));
    wartend = {};
    return;
  }
  const p = wartend[m.type];
  if (p) { delete wartend[m.type]; p.res(m); }
};
const frage = (msg, typ, transfer = []) => new Promise((res, rej) => { wartend[typ] = { res, rej }; worker.postMessage(msg, transfer); });

// ---------- Datei laden ----------
function zeige(id) {
  for (const s of ['start', 'laden', 'ergebnis']) $('#' + s).classList.toggle('hidden', s !== id);
  $('#neu').classList.toggle('hidden', id !== 'ergebnis');
}
function ladeStand(text, p) {
  if (text) $('#laden-text').textContent = text;
  if (p != null) $('#laden-bar').style.width = Math.round(p * 100) + '%';
}

function kopfInfo(ab, file) {
  const u8 = new Uint8Array(ab, 0, Math.min(ab.byteLength, 4096));
  const tag = (o, s) => [...s].every((c, i) => u8[o + i] === c.charCodeAt(0));
  const ext = (file.name.split('.').pop() || '').toUpperCase();
  if (tag(0, 'RIFF') && tag(8, 'WAVE')) {
    const dv = new DataView(ab);
    let o = 12;
    while (o + 8 < Math.min(ab.byteLength, 1 << 20)) {
      const id = String.fromCharCode(...new Uint8Array(ab, o, 4));
      const len = dv.getUint32(o + 4, true);
      if (id === 'fmt ') return { typ: 'WAV', rate: dv.getUint32(o + 12, true), bits: dv.getUint16(o + 22, true) };
      o += 8 + len + (len & 1);
    }
    return { typ: 'WAV' };
  }
  if (tag(0, 'fLaC')) return { typ: 'FLAC' };
  if (tag(0, 'OggS')) return { typ: ext === 'OPUS' ? 'Opus' : 'OGG' };
  if (tag(4, 'ftyp')) return { typ: 'M4A' };
  if (tag(0, 'ID3') || (u8[0] === 0xff && (u8[1] & 0xe0) === 0xe0)) return { typ: 'MP3', verlustbehaftet: true };
  return { typ: ext || 'Audio' };
}

async function lade(file) {
  if (!file) return;
  stop();
  zeige('laden');
  ladeStand('Datei wird gelesen …', 0.02);
  try {
    const ab = await file.arrayBuffer();
    const kopf = kopfInfo(ab, file);
    S.ctx = S.ctx || new (window.AudioContext || window.webkitAudioContext)();
    ladeStand('Audio wird entpackt …', 0.04);
    const buf = await new Promise((res, rej) => S.ctx.decodeAudioData(ab.slice(0), res, rej));
    const chs = [];
    for (let c = 0; c < buf.numberOfChannels; c++) chs.push(buf.getChannelData(c).slice());
    ladeStand('Lautheit, Tempo und Schnitte werden analysiert …');
    fortschritt = (p) => ladeStand(null, 0.05 + p * 0.95);
    const m = await frage({ cmd: 'analyse', channels: chs, rate: buf.sampleRate }, 'analysed', chs.map((c) => c.buffer));
    const r = m.res;
    Object.assign(S, {
      file, kopf, buf, rate: buf.sampleRate, dur: buf.duration, nf: r.nf,
      fr: r.frames, info: r.info, cuts: r.cuts, autoCuts: r.cuts.map((c) => ({ ...c })),
      view: { t0: 0, t1: buf.duration }, sel: -1, mode: 'orig', offset: 0, levelled: null, levelKey: '',
      grids: new Map(),
    });
    S.fr.st = K.shortTerm(S.fr.z);
    S.zp = K.prefix(S.fr.z);
    S.gesamt = K.integrated(S.fr.z, 0, S.nf);
    const b0 = grenzen();
    r.grids.forEach((g, i) => S.grids.set(b0[i] + '-' + b0[i + 1], g));
    neuBerechnen(true);
    $('#m-orig').classList.add('on'); $('#m-neu').classList.remove('on');
    korrekturStatus(null);
    if (S.ansicht === 'einfach') S.view = { t0: 0, t1: S.dur };
    zeige('ergebnis');
    S.dirty = true;
    requestAnimationFrame(male);
  } catch (err) {
    console.error(err);
    zeige('start');
    toast('Die Datei konnte nicht gelesen werden. Ist es eine Audiodatei?', 5000);
  }
}

// ---------- Teile auswerten ----------
const grenzen = () => [0, ...S.cuts.map((c) => c.f), S.nf];

function gewichteterMedian(items) {
  const v = items.filter((x) => isFinite(x.v)).sort((a, b) => a.v - b.v);
  const tot = v.reduce((s, x) => s + x.w, 0);
  let acc = 0;
  for (const x of v) { acc += x.w; if (acc >= tot / 2) return x.v; }
  return NaN;
}

function neuBerechnen(aufraeumen = false) {
  S.cuts.sort((a, b) => a.f - b.f);
  const teile = () => { const b = grenzen(); S.segs = []; for (let i = 0; i + 1 < b.length; i++) S.segs.push(K.segStats(S.fr, b[i], b[i + 1])); };
  teile();
  if (aufraeumen) {
    // Lautstärkesprünge ohne echten Unterschied zwischen den Teilen verwerfen
    for (let again = true; again;) {
      again = false;
      for (let i = 0; i < S.cuts.length; i++) {
        if (S.cuts[i].why !== 'pegel') continue;
        const d = Math.abs(S.segs[i + 1].lufs - S.segs[i].lufs);
        if (!(d >= 2.5)) { S.cuts.splice(i, 1); teile(); again = true; break; }
      }
    }
    S.autoCuts = S.cuts.map((c) => ({ ...c }));
  }
  const rel = S.segs.filter((s) => s.dur >= 4 && s.lufs > -45);
  S.ref = gewichteterMedian(rel.map((s) => ({ v: s.lufs, w: s.dur })));
  if (!isFinite(S.ref)) S.ref = S.gesamt;
  for (const s of S.segs) {
    s.d = s.lufs - S.ref;
    s.rate = s.clipEv / Math.max(s.dur / 60, 0.1);
    if (!(s.lufs > -45) || s.dur < 1) s.lvl = 'leer';
    else if (Math.abs(s.d) > 4 || (s.clipEv >= 3 && s.rate > 20)) s.lvl = 'bad';
    else if (Math.abs(s.d) > 2 || (s.clipEv >= 3 && s.rate > 2)) s.lvl = 'warn';
    else s.lvl = 'ok';
    s.grid = S.grids.get(s.a + '-' + s.b);
  }
  const warKorrigiert = !!S.levelled;
  S.levelled = null; S.levelKey = '';
  korrekturStatus(warKorrigiert ? 'laeuft' : null, 'Schnitte geändert – Korrektur wird neu berechnet …');
  if (S.mode === 'neu' || warKorrigiert) modus(S.mode, true, warKorrigiert);
  S.dirty = true;
  zeichneAlles();
  tempoNachladen();
}

let tempoId = 0;
async function tempoNachladen() {
  const fehlt = S.segs.filter((s) => !S.grids.has(s.a + '-' + s.b));
  if (!fehlt.length) return;
  const id = ++tempoId;
  try {
    const m = await frage({ cmd: 'bpm', id, segs: fehlt.map((s) => [s.a, s.b]) }, 'bpm');
    fehlt.forEach((s, i) => S.grids.set(s.a + '-' + s.b, m.grids[i]));
    if (id !== tempoId) return;
    for (const s of S.segs) s.grid = S.grids.get(s.a + '-' + s.b);
    zeichneAlles();
  } catch (e) { console.warn(e); }
}

// ---------- Tipps ----------
function teilName(i) {
  const s = S.segs[i];
  return `<a data-seg="${i}">Teil ${i + 1}</a> (${zeit(s.a / FPS)}–${zeit(s.b / FPS)})`;
}

function tippsBerechnen() {
  const T = [];
  const add = (lvl, html) => T.push({ lvl, html });
  const rel = S.segs.map((s, i) => ({ s, i })).filter((x) => x.s.lvl !== 'leer');
  const lauf = rel.map((x) => x.s.lufs);
  const spanne = lauf.length > 1 ? Math.max(...lauf) - Math.min(...lauf) : 0;
  const ziel = zielLufs();

  // Lautstärke der Teile
  for (const { s, i } of rel) {
    if (Math.abs(s.d) <= 2) continue;
    const lvl = Math.abs(s.d) > 4 ? 'bad' : 'warn';
    const neuePeak = s.peak - s.d;
    let t = `${teilName(i)} ist ${zahl(Math.abs(s.d))} dB ${s.d < 0 ? 'zu leise' : 'zu laut'} → um <b>${plus(-s.d)} dB</b> ${s.d < 0 ? 'anheben' : 'absenken'}.`;
    if (s.d < 0 && neuePeak > -1) t += ` Die Spitzen kämen dann auf ${plus(neuePeak)} dBFS – nur mit Limiter anheben (die angeglichene Fassung macht das automatisch).`;
    add(lvl, t);
  }
  // Übersteuerung
  for (const { s, i } of rel) {
    if (s.clipEv < 3 || s.rate <= 2) continue;
    add(s.rate > 20 ? 'bad' : 'warn',
      `${teilName(i)} ist übersteuert: ${s.clipEv.toLocaleString('de-DE')} Stellen mit abgeschnittenen Spitzen. ` +
      `Leiser machen hilft gegen die Verzerrung nicht – den Teil aus der Originaldatei neu einsetzen und beim Zusammenschneiden nicht über 0 dBFS gehen.`);
  }
  // Sprünge an den Schnitten
  let spruenge = 0;
  S.cuts.forEach((c, k) => {
    const a = S.segs[k], b = S.segs[k + 1];
    if (!a || !b || a.lvl === 'leer' || b.lvl === 'leer' || a.lvl !== 'ok' || b.lvl !== 'ok' || spruenge >= 3) return;
    const d = K.meanDb(S.zp, c.f, c.f + 150) - K.meanDb(S.zp, c.f - 150, c.f);
    if (Math.abs(d) >= 6 && isFinite(d)) {
      spruenge++;
      add('warn', `Beim Schnitt bei <a data-t="${c.f / FPS}">${zeit(c.f / FPS, true)}</a> springt die Lautstärke um ${plus(d)} LU – Übergang einblenden oder die Teile vorher angleichen.`);
    }
  });
  if (spanne > 2) add('info', `Arbeitsweise: Jeden Teil <b>vor</b> dem Schneiden auf dieselbe Lautheit bringen (z. B. in Audacity mit „Lautheit normalisieren“ auf ${zahl(Math.round(ziel), 0)} LUFS), erst dann zusammensetzen und am Ende einen Limiter auf −1 dBFS legen.`);

  // ganze Datei
  if (S.info.peak > -0.1 && !rel.some((x) => x.s.rate > 2)) add('warn', `Die Spitzen erreichen ${S.info.peak > 0 ? 'mehr als ' : ''}0 dBFS. Beim Export einen Limiter mit −1 dBFS Obergrenze verwenden, sonst kann es auf der Anlage verzerren.`);
  if (S.info.overs > 50) add('warn', `${S.info.overs.toLocaleString('de-DE')} Samples liegen über 0 dBFS (typisch für zu laut gespeicherte MP3s). Die Datei vor dem Export etwas absenken.`);
  if (S.gesamt > -7) add('warn', `Die Datei ist sehr laut gemastert (${zahl(S.gesamt)} LUFS). Das klingt auf großen Anlagen schnell anstrengend und verzerrt – ca. −10 LUFS reichen.`);
  else if (S.gesamt < -20) add('warn', `Die Datei ist insgesamt sehr leise (${zahl(S.gesamt)} LUFS). Der Techniker muss weit aufdrehen – auf etwa −14 bis −10 LUFS bringen.`);
  if (S.info.lead > 1.5) add('warn', `${zahl(S.info.lead)} s Stille am Anfang – für den Einsatz auf Kommando auf höchstens 0,5 s kürzen.`);
  else if (S.info.lead > 0.6) add('info', `${zahl(S.info.lead)} s Stille am Anfang. Wenn die Musik sofort einsetzen soll, kürzen.`);
  if (S.info.trail > 4) add('info', `${zahl(S.info.trail)} s Stille am Ende – kann gekürzt werden.`);
  if (S.info.channels === 2) {
    const [l, r] = S.info.rms;
    if (Math.abs(l - r) > 3 && isFinite(l) && isFinite(r)) add('warn', `Links und rechts sind unterschiedlich laut (${zahl(Math.abs(l - r))} dB). Der ${l < r ? 'linke' : 'rechte'} Kanal ist leiser.`);
    if (S.info.corr != null && S.info.corr < 0) add('bad', `Die Kanäle arbeiten gegeneinander (Phase, Korrelation ${zahl(S.info.corr, 2)}). Auf Mono-Anlagen in Hallen können dann Teile fast verschwinden.`);
  } else if (S.info.channels === 1) add('info', 'Die Datei ist mono. Das ist für Hallenanlagen in Ordnung.');
  if (S.info.dc.some((d) => Math.abs(d) > 0.01)) add('info', 'Die Datei hat einen Gleichspannungsanteil (DC-Offset). Mit einem Hochpass oder „DC-Offset entfernen“ beheben.');
  if (S.kopf.typ === 'MP3') {
    const kbit = S.file.size * 8 / S.dur / 1000;
    if (kbit < 170) add('warn', `Niedrige MP3-Qualität (ca. ${Math.round(kbit)} kbit/s). Für große Anlagen besser 320 kbit/s oder WAV.`);
  }
  if (!T.some((t) => t.lvl === 'bad' || t.lvl === 'warn')) add('ok', 'Lautstärke und Spitzen sind gleichmäßig. Die Datei kann so verwendet werden.');
  const rang = { bad: 0, warn: 1, info: 2, ok: 3 };
  T.sort((a, b) => rang[a.lvl] - rang[b.lvl]);
  return T;
}

// ---------- Einfache Sprache ----------
const verzerrt = (s) => s.clipEv >= 3 && s.rate > 2;
function wort(s) {
  if (s.lvl === 'leer') return 'Stille';
  const w = s.d < -4 ? 'viel zu leise' : s.d < -2 ? 'etwas zu leise' : s.d > 4 ? 'viel zu laut' : s.d > 2 ? 'etwas zu laut' : '';
  if (verzerrt(s)) return w ? w + ', verzerrt' : 'verzerrt';
  return w || 'passt';
}
function einfacheTipps() {
  const T = [];
  const add = (lvl, html, auto) => T.push({ lvl, html, auto });
  const kurz = $('#trimmen').checked;
  S.segs.forEach((s, i) => {
    if (s.lvl === 'leer') return;
    const name = `<a data-seg="${i}">Teil ${i + 1}</a> (${zeit(s.a / FPS)}–${zeit(s.b / FPS)})`;
    if (s.d < -2) add(s.d < -4 ? 'bad' : 'warn', `${name} ist ${s.d < -4 ? 'viel' : 'etwas'} leiser als der Rest. Im Saal geht er unter, wenn die Anlage für die anderen Teile eingestellt ist.`, true);
    if (s.d > 2) add(s.d > 4 ? 'bad' : 'warn', `${name} ist ${s.d > 4 ? 'viel' : 'etwas'} lauter als der Rest. Das Publikum erschrickt, der Techniker muss nachregeln.`, true);
    if (verzerrt(s)) add(s.rate > 20 ? 'bad' : 'warn', `${name} klingt kratzig und verzerrt, weil er zu laut abgespeichert wurde. Das kann keine Korrektur reparieren – das Lied am besten in guter Qualität neu besorgen und neu einschneiden.`, false);
  });
  if (S.info.lead > 1.5) add('warn', `Am Anfang sind ${zahl(S.info.lead)} Sekunden Stille. Die Musik startet dann nicht sofort auf Kommando.`, kurz);
  if (S.gesamt > -7) add('warn', 'Die Datei ist insgesamt sehr laut gemacht. Auf großen Anlagen klingt das schnell anstrengend.', true);
  else if (S.gesamt < -20) add('warn', 'Die Datei ist insgesamt sehr leise. Der Techniker muss sehr weit aufdrehen.', true);
  if (S.info.channels === 2 && S.info.corr != null && S.info.corr < 0) add('bad', 'Links und rechts arbeiten gegeneinander. Auf manchen Hallenanlagen sind Teile der Musik dann kaum zu hören.', false);
  if (S.info.channels === 2) { const [l, r] = S.info.rms; if (Math.abs(l - r) > 3 && isFinite(l) && isFinite(r)) add('warn', `Der ${l < r ? 'linke' : 'rechte'} Lautsprecher ist deutlich leiser als der andere.`, false); }
  if (S.kopf.typ === 'MP3' && S.file.size * 8 / S.dur / 1000 < 170) add('warn', 'Die Datei hat eine niedrige Qualität. Auf großen Boxen klingt sie dumpf. Besser eine Datei mit 320 kbit/s oder WAV besorgen.', false);
  if (!T.length) add('ok', 'Alle Teile sind gleich laut und nichts ist übersteuert. Die Musik kann so verwendet werden.', null);
  const rang = { bad: 0, warn: 1, info: 2, ok: 3 };
  T.sort((a, b) => rang[a.lvl] - rang[b.lvl]);
  return T;
}
function einfachesUrteil(tipps) {
  const teile = S.segs.filter((s) => s.lvl !== 'leer');
  const laut = teile.filter((s) => Math.abs(s.d) > 2).length, kaputt = teile.filter(verzerrt).length;
  const schlecht = tipps.some((t) => t.lvl === 'bad'), warn = tipps.some((t) => t.lvl === 'warn');
  const lvl = schlecht ? 'bad' : warn ? 'warn' : 'ok';
  const kopf = { bad: 'Diese Musik sollte vor dem Auftritt überarbeitet werden.', warn: 'Fast gut – ein paar Kleinigkeiten.', ok: 'Alles in Ordnung – die Musik kann so verwendet werden.' }[lvl];
  const teil = [];
  if (laut) teil.push(`${laut} von ${teile.length} Teilen ${laut === 1 ? 'ist' : 'sind'} zu leise oder zu laut`);
  if (kaputt) teil.push(`${kaputt} ${kaputt === 1 ? 'Teil klingt' : 'Teile klingen'} verzerrt`);
  let sub = teil.length ? teil.join(', ') + '. ' : `Die Musik besteht aus ${teile.length} ${teile.length === 1 ? 'Teil' : 'Teilen'}. `;
  if (tipps.some((t) => t.auto)) sub += 'Die Lautstärke behebt die automatische Korrektur mit einem Klick.';
  if (tipps.some((t) => t.auto === false)) sub += (tipps.some((t) => t.auto) ? ' ' : '') + 'Einiges muss von Hand gemacht werden.';
  return `<div class="gross">${{ bad: '✕', warn: '!', ok: '✓' }[lvl]}</div><div><b>${kopf}</b><p>${sub}</p></div>`;
}

// ---------- Anzeige ----------
function zeichneAlles() {
  if (!S.segs) return;
  const k = S.kopf, kanaele = { 1: 'Mono', 2: 'Stereo' }[S.info.channels] || S.info.channels + ' Kanäle';
  const teile = [k.typ];
  if (k.bits) teile.push(k.bits + ' Bit');
  if (k.rate) teile.push(zahl(k.rate / 1000, k.rate % 1000 ? 1 : 0) + ' kHz');
  if (k.typ === 'MP3') teile.push('ca. ' + Math.round(S.file.size * 8 / S.dur / 1000) + ' kbit/s');
  teile.push(kanaele, zeit(S.dur), (S.file.size / 1048576).toFixed(1).replace('.', ',') + ' MB');
  $('#name').textContent = S.file.name;
  $('#format').textContent = teile.join(' · ');

  const einfach = S.ansicht === 'einfach';
  const tipps = einfach ? einfacheTipps() : tippsBerechnen();
  if (einfach) {
    const e = $('#e-urteil');
    e.className = 'e-urteil ' + (tipps.some((t) => t.lvl === 'bad') ? 'bad' : tipps.some((t) => t.lvl === 'warn') ? 'warn' : 'ok');
    e.innerHTML = einfachesUrteil(tipps);
  }
  const schlecht = tipps.some((t) => t.lvl === 'bad'), warn = tipps.some((t) => t.lvl === 'warn');
  const u = $('#urteil');
  u.className = 'urteil nur-profi ' + (schlecht ? 'bad' : warn ? 'warn' : 'ok');
  u.textContent = schlecht ? '✕ Überarbeiten' : warn ? '! Bitte prüfen' : '✓ Einsatzbereit';
  const marke = (t) => t.auto == null ? '' : t.auto ? ' <span class="marke auto">wird automatisch behoben</span>' : ' <span class="marke hand">von Hand nötig</span>';
  $('#tipps').innerHTML = tipps.map((t) => `<li class="${t.lvl}"><span>${{ bad: '✕', warn: '!', info: 'i', ok: '✓' }[t.lvl]}</span><span>${t.html}${einfach ? marke(t) : ''}</span></li>`).join('');

  // Kennzahlen
  const rel = S.segs.filter((s) => s.lvl !== 'leer');
  const lauf = rel.map((s) => s.lufs);
  const spanne = lauf.length > 1 ? Math.max(...lauf) - Math.min(...lauf) : 0;
  const clips = S.segs.reduce((a, s) => a + s.clipEv, 0);
  const proMin = clips / Math.max(S.dur / 60, 0.1);
  const z = [
    ['Gesamtlautheit', zahl(S.gesamt), 'LUFS', S.gesamt > -7 || S.gesamt < -20 ? 'warn' : ''],
    ['Unterschied der Teile', zahl(spanne), 'LU', spanne > 6 ? 'bad' : spanne > 3 ? 'warn' : 'ok'],
    ['Höchste Spitze', plus(S.info.peak), 'dBFS', S.info.peak > -0.1 ? 'warn' : 'ok'],
    ['Übersteuerungen', clips.toLocaleString('de-DE'), clips === 1 ? 'Stelle' : 'Stellen', clips >= 3 && proMin > 20 ? 'bad' : clips >= 3 && proMin > 2 ? 'warn' : 'ok'],
    ['Teile', String(S.segs.length), 'Ø ' + zeit(S.dur / S.segs.length), ''],
  ];
  $('#zahlen').innerHTML = z.map(([t, v, e, c]) => `<div class="zahl ${c}"><small>${t}</small><b>${v}</b><em>${e}</em></div>`).join('');

  // Aufbau-Leiste
  $('#aufbau').innerHTML = S.segs.map((s, i) => {
    const g = s.grid ? ' · ' + Math.round(s.grid.bpm) + ' BPM' : '';
    if (einfach) return `<div class="${s.lvl}${i === S.sel ? ' sel' : ''}" style="flex:${s.dur}" data-seg="${i}" title="Teil ${i + 1}: ${zeit(s.a / FPS)}–${zeit(s.b / FPS)}, ${wort(s)}"><b>Teil ${i + 1}</b>${wort(s)}</div>`;
    return `<div class="${s.lvl}${i === S.sel ? ' sel' : ''}" style="flex:${s.dur}" data-seg="${i}" title="Teil ${i + 1}: ${zeit(s.a / FPS)}–${zeit(s.b / FPS)}"><b>${i + 1}</b>${zeit(s.dur)}${g}</div>`;
  }).join('');

  // Tabelle
  const grund = (i) => {
    if (i === 0) return 'Anfang';
    const c = S.cuts[i - 1];
    if (c.why === 'tempo') return `Tempowechsel ${c.info[0]}→${c.info[1]}`;
    if (c.why === 'pause') return `Pause ${zahl(c.info / 1000)} s`;
    if (c.why === 'pegel') return `Lautstärkesprung ${plus(c.info)} LU`;
    return 'von Hand';
  };
  const balken = (d) => {
    if (!isFinite(d)) return '';
    const w = Math.min(Math.abs(d), 8) / 8 * 50;
    const c = Math.abs(d) > 4 ? FARBE.bad : Math.abs(d) > 2 ? FARBE.warn : FARBE.ok;
    return `<span class="abw"><i style="${d < 0 ? 'right:50%' : 'left:50%'};width:${w}%;background:${c}"></i></span>`;
  };
  $('#teile').innerHTML = `<thead><tr><th>Teil</th><th class="weg">Zeit</th><th class="r">Dauer</th><th class="r weg">Tempo</th><th class="r">Lautheit</th><th>Abweichung</th><th class="r">Spitze</th><th class="r">Übersteuert</th><th class="weg">Schnitt</th><th></th></tr></thead><tbody>` +
    S.segs.map((s, i) => {
      const dc = s.lvl === 'leer' ? '' : Math.abs(s.d) > 4 ? 'c-bad' : Math.abs(s.d) > 2 ? 'c-warn' : '';
      const cc = s.clipEv >= 3 && s.rate > 20 ? 'c-bad' : s.clipEv >= 3 && s.rate > 2 ? 'c-warn' : 'dim';
      return `<tr data-seg="${i}" class="${i === S.sel ? 'sel' : ''}">
        <td><span class="punkt ${s.lvl}"></span>${i + 1}</td>
        <td class="weg">${zeit(s.a / FPS)}–${zeit(s.b / FPS)}</td>
        <td class="r">${zeit(s.dur)}</td>
        <td class="r weg">${s.grid ? Math.round(s.grid.bpm) + ' BPM' : '<span class="dim">–</span>'}</td>
        <td class="r">${s.lvl === 'leer' ? '<span class="dim">Stille</span>' : zahl(s.lufs) + ' <span class="dim">LUFS</span>'}</td>
        <td class="${dc}">${s.lvl === 'leer' ? '' : balken(s.d) + plus(s.d)}</td>
        <td class="r ${s.peak > -0.1 ? 'c-warn' : ''}">${plus(s.peak)}</td>
        <td class="r ${cc}">${s.clipEv ? s.clipEv.toLocaleString('de-DE') + '×' : '–'}</td>
        <td class="weg warum">${grund(i)}</td>
        <td class="r"><button class="mini" data-play="${i}" title="Teil abspielen">▶</button>${i > 0 ? `<button class="mini" data-weg="${i - 1}" title="Schnitt davor entfernen">✕</button>` : ''}</td>
      </tr>`;
    }).join('') + '</tbody>';
  S.dirty = true;
  male();
}

// ---------- Zeichnen ----------
const cvW = $('#welle'), cvK = $('#kurve');
const ebene = { w: document.createElement('canvas'), k: document.createElement('canvas') };

function passe(cv, off) {
  const r = cv.getBoundingClientRect(), d = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(r.width * d)), h = Math.max(1, Math.round(r.height * d));
  for (const c of [cv, off]) if (c.width !== w || c.height !== h) { c.width = w; c.height = h; S.dirty = true; }
  return { w: r.width, h: r.height, d };
}
const tx = (t, w) => (t - S.view.t0) / (S.view.t1 - S.view.t0) * w;
const xt = (x, w) => S.view.t0 + x / w * (S.view.t1 - S.view.t0);
const WTOP = 20, WBOT = 16;

// In der Ansicht „Korrigiert“: Verstärkung je Teil (dB), sonst null
function anzeigeGain() {
  if (S.mode !== 'neu' || !S.levelled) return null;
  return verstaerkungen().map((x) => x.db);
}
function zeichneWelle(g, w, h) {
  const gains = anzeigeGain();
  const top = gains ? Math.max(...S.segs.map((s, i) => (s.lvl === 'leer' ? -Infinity : s.lufs + gains[i]))) : 0;
  const maxLufs = Math.max(...S.segs.filter((s) => s.lvl !== 'leer').map((s) => s.lufs));
  let skal = gains ? S.segs.map((s, i) => Math.pow(10, (gains[i] - (top - maxLufs)) / 20)) : null;
  if (skal) {
    // so skalieren, dass der kräftigste Teil die Höhe gerade füllt
    let m = 0;
    S.segs.forEach((s, i) => {
      if (s.lvl === 'leer') return;
      const v = Array.from(S.fr.low.subarray(s.a, s.b)).sort((x, y) => x - y);
      m = Math.max(m, (v[Math.floor(v.length * 0.98)] || 0) * skal[i]);
    });
    if (m > 0) skal = skal.map((k) => k * 0.95 / m);
  }
  g.fillStyle = '#0b0e12'; g.fillRect(0, 0, w, h);
  const wh = h - WTOP - WBOT, mid = WTOP + wh / 2, half = wh * 0.48;
  // Teile-Streifen
  g.font = '600 11px system-ui, sans-serif'; g.textBaseline = 'middle';
  S.segs.forEach((s, i) => {
    const x0 = tx(s.a / FPS, w), x1 = tx(s.b / FPS, w);
    if (x1 < 0 || x0 > w) return;
    g.fillStyle = gains ? (s.lvl === 'leer' ? STREIFEN.leer : verzerrt(s) ? STREIFEN.warn : STREIFEN.ok) : STREIFEN[s.lvl]; g.fillRect(x0, 0, x1 - x0, WTOP - 2);
    if (i === S.sel) { g.fillStyle = 'rgba(25,227,240,0.07)'; g.fillRect(x0, WTOP, x1 - x0, wh); }
    g.save(); g.beginPath(); g.rect(x0, 0, x1 - x0, WTOP); g.clip();
    g.fillStyle = gains ? (s.lvl === 'leer' ? FARBE.leer : verzerrt(s) ? FARBE.warn : FARBE.ok) : FARBE[s.lvl];
    const lab = gains ? (S.ansicht === 'einfach'
      ? `Teil ${i + 1} · ${s.lvl === 'leer' ? 'Stille' : verzerrt(s) ? 'gleich laut, bleibt verzerrt' : 'korrigiert'}`
      : `${i + 1} · ${s.lvl === 'leer' ? 'Stille' : zahl(s.lufs + gains[i]) + ' LUFS (' + plus(gains[i]) + ' dB)'}`)
      : S.ansicht === 'einfach' ? `Teil ${i + 1} · ${wort(s)}` : s.lvl === 'leer' ? `${i + 1} · Stille` : `${i + 1} · ${zahl(s.lufs)} LUFS${Math.abs(s.d) > 2 ? ' (' + plus(s.d) + ')' : ''}`;
    g.fillText(x1 - x0 > 90 ? lab : String(i + 1), Math.max(x0, 0) + 6, WTOP / 2 - 1);
    g.restore();
  });
  // Bänder wie in DMXCraft: Bass außen, Mitten, Höhen innen
  const fpp = (S.view.t1 - S.view.t0) * FPS / w;
  const f0v = S.view.t0 * FPS;
  const { low, mid: md, high, clipEv } = S.fr;
  const step = 1;
  for (let x = 0; x < w; x += step) {
    const a = Math.max(0, Math.floor(f0v + x * fpp)), b = Math.min(S.nf, Math.max(a + 1, Math.floor(f0v + (x + step) * fpp)));
    if (a >= S.nf) break;
    let l = 0, m = 0, hi = 0, c = 0;
    for (let i = a; i < b; i++) { if (low[i] > l) l = low[i]; if (md[i] > m) m = md[i]; if (high[i] > hi) hi = high[i]; c += clipEv[i]; }
    if (skal) {
      const k = skal[segAnFrame(a)] || 1;
      l = Math.min(1, l * k); m = Math.min(1, m * k); hi = Math.min(1, hi * k);
    }
    if (c) { g.fillStyle = 'rgba(239,68,85,0.22)'; g.fillRect(x, WTOP, step, wh); }
    const bar = (v, sc, col) => { const hh = v * half * sc; if (hh >= 0.5) { g.fillStyle = col; g.fillRect(x, mid - hh, step, hh * 2); } };
    bar(l, 1, '#2f80ff'); bar(m, 0.7, '#f5a524'); bar(hi, 0.45, '#eef2ff');
    if (c) { g.fillStyle = '#ef4455'; g.fillRect(x, WTOP + wh - 5, Math.max(step, 2), 5); }
  }
  // Schnitte
  S.cuts.forEach((c, i) => {
    const x = Math.round(tx(c.f / FPS, w)) + 0.5;
    if (x < -10 || x > w + 10) return;
    const hot = i === S.hoverCut || i === S.drag;
    g.strokeStyle = hot ? '#ffffff' : '#19e3f0'; g.lineWidth = hot ? 2 : 1.5;
    g.beginPath(); g.moveTo(x, 0); g.lineTo(x, WTOP + wh); g.stroke();
    g.fillStyle = hot ? '#ffffff' : '#19e3f0';
    g.beginPath(); g.moveTo(x - 6, WTOP - 2); g.lineTo(x + 6, WTOP - 2); g.lineTo(x, WTOP + 6); g.closePath(); g.fill();
  });
  // Zeitleiste
  const span = S.view.t1 - S.view.t0;
  const schritt = [0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300].find((s) => s / span * w >= 64) || 600;
  g.fillStyle = '#55606c'; g.font = '11px system-ui, sans-serif'; g.textBaseline = 'alphabetic';
  g.fillRect(0, h - WBOT, w, 1);
  for (let t = Math.ceil(S.view.t0 / schritt) * schritt; t <= S.view.t1; t += schritt) {
    const x = Math.round(tx(t, w)) + 0.5;
    g.fillRect(x, h - WBOT, 1, 4);
    g.fillText(schritt < 1 ? zeit(t, true) : zeit(t), x + 3, h - 3);
  }
}

const YMIN = -36, YMAX = 0;
function zeichneKurve(g, w, h) {
  g.fillStyle = '#0b0e12'; g.fillRect(0, 0, w, h);
  const top = 8, H = h - 16;
  const y = (db) => top + (YMAX - Math.max(YMIN, Math.min(YMAX, db))) / (YMAX - YMIN) * H;
  g.font = '10.5px system-ui, sans-serif'; g.textBaseline = 'middle';
  for (let db = 0; db >= YMIN; db -= 6) {
    g.fillStyle = '#1a2028'; g.fillRect(0, Math.round(y(db)), w, 1);
    if (db > YMIN) { g.fillStyle = '#55606c'; g.fillText(db ? minus(String(db)) : '0 dB', 4, y(db) + 7); }
  }
  // Zielbereich
  const ziel = zielLufs();
  g.fillStyle = 'rgba(52,211,153,0.12)'; g.fillRect(0, y(ziel + 2), w, y(ziel - 2) - y(ziel + 2));
  g.strokeStyle = 'rgba(52,211,153,0.6)'; g.setLineDash([4, 4]); g.lineWidth = 1;
  g.beginPath(); g.moveTo(0, y(ziel) + 0.5); g.lineTo(w, y(ziel) + 0.5); g.stroke(); g.setLineDash([]);

  const fpp = (S.view.t1 - S.view.t0) * FPS / w, f0v = S.view.t0 * FPS;
  const gains = anzeigeGain();
  const gAt = (f) => (gains ? gains[segAnFrame(f)] : 0);
  // Spitzenpegel
  g.strokeStyle = 'rgba(239,68,85,0.55)'; g.lineWidth = 1; g.beginPath();
  for (let x = 0; x < w; x++) {
    const c = f0v + (x + 0.5) * fpp, r = Math.max(fpp / 2, 25);
    const a = Math.max(0, Math.floor(c - r)), b = Math.min(S.nf, Math.max(a + 1, Math.ceil(c + r)));
    if (a >= S.nf) break;
    let p = 0; for (let i = a; i < b; i++) if (S.fr.peak[i] > p) p = S.fr.peak[i];
    const yy = y(gains ? Math.min(-1, K.dbfs(p) + gAt(a)) : K.dbfs(p));
    x ? g.lineTo(x, yy) : g.moveTo(x, yy);
  }
  g.stroke();
  // Lautheit je Teil (Balken)
  S.segs.forEach((s, si) => {
    if (s.lvl === 'leer') return;
    const x0 = tx(s.a / FPS, w), x1 = tx(s.b / FPS, w);
    if (x1 < 0 || x0 > w) return;
    g.fillStyle = gains ? FARBE.ok : FARBE[s.lvl]; g.globalAlpha = 0.9;
    g.fillRect(x0 + 2, y(s.lufs + (gains ? gains[si] : 0)) - 2, Math.max(1, x1 - x0 - 4), 4);
    g.globalAlpha = 1;
  });
  // Kurzzeit-Lautheit
  g.strokeStyle = '#19e3f0'; g.lineWidth = 1.6; g.beginPath();
  const st = S.fr.st;
  for (let x = 0; x < w; x++) {
    const j = Math.min(st.length - 1, Math.max(0, Math.round((f0v + x * fpp) / 10)));
    const yy = y(st[j] + gAt(j * 10));
    x ? g.lineTo(x, yy) : g.moveTo(x, yy);
  }
  g.stroke();
  // Ziel beschriften (mit Hintergrund, damit es lesbar bleibt)
  const lab = `Ziel ${zahl(ziel)} LUFS`;
  g.font = '600 11px system-ui, sans-serif';
  const lw = g.measureText(lab).width + 10, ly = Math.max(top, y(ziel + 2) - 18);
  g.fillStyle = 'rgba(11,14,18,0.85)'; g.fillRect(w - lw - 4, ly, lw, 16);
  g.fillStyle = '#34d399'; g.fillText(lab, w - lw + 1, ly + 8);
  // Schnitte
  g.strokeStyle = 'rgba(25,227,240,0.35)'; g.lineWidth = 1;
  for (const c of S.cuts) { const x = Math.round(tx(c.f / FPS, w)) + 0.5; g.beginPath(); g.moveTo(x, 0); g.lineTo(x, h); g.stroke(); }
}

function male() {
  if (!S.segs || $('#ergebnis').classList.contains('hidden')) return;
  const a = passe(cvW, ebene.w), b = passe(cvK, ebene.k);
  if (S.dirty) {
    for (const [off, info, fn] of [[ebene.w, a, zeichneWelle], [ebene.k, b, zeichneKurve]]) {
      const g = off.getContext('2d');
      g.setTransform(info.d, 0, 0, info.d, 0, 0);
      fn(g, info.w, info.h);
    }
    S.dirty = false;
  }
  const t = pos();
  for (const [cv, off, info] of [[cvW, ebene.w, a], [cvK, ebene.k, b]]) {
    const g = cv.getContext('2d');
    g.setTransform(1, 0, 0, 1, 0, 0);
    g.drawImage(off, 0, 0);
    g.setTransform(info.d, 0, 0, info.d, 0, 0);
    const x = tx(t, info.w);
    if (x >= 0 && x <= info.w) { g.fillStyle = '#ffffff'; g.fillRect(Math.round(x) - 1, 0, 2, info.h); }
  }
  $('#zeit').textContent = zeit(t) + ' / ' + zeit(S.dur);
}

// ---------- Wiedergabe ----------
function pos() {
  if (!S.ctx) return 0;
  return S.playing ? Math.min(S.dur, S.ctx.currentTime - S.startedAt) : S.offset;
}
function play(from) {
  if (from != null) S.offset = Math.max(0, Math.min(S.dur - 0.01, from));
  stop(true);
  S.ctx.resume();
  const src = S.ctx.createBufferSource();
  src.buffer = S.mode === 'neu' && S.levelled ? S.levelled : S.buf;
  if (!S.gain) { S.gain = S.ctx.createGain(); S.gain.connect(S.ctx.destination); lautstaerke(); }
  src.connect(S.gain);
  // Die korrigierte Fassung kann vorne gekürzt sein: Zeit bleibt die des Originals
  const tr = src.buffer === S.levelled ? S.levelTrim : 0;
  const bo = Math.max(0, S.offset - tr);
  src.start(0, bo);
  S.src = src; S.startedAt = S.ctx.currentTime - (bo + tr); S.playing = true;
  src.onended = () => {
    if (S.src !== src) return;
    S.playing = false; S.offset = 0; S.src = null;
    $('#play').classList.remove('laeuft'); male();
  };
  $('#play').classList.add('laeuft');
  const lauf = () => {
    if (!S.playing) return;
    const t = pos(), span = S.view.t1 - S.view.t0;
    if (span < S.dur - 0.01 && (t > S.view.t1 || t < S.view.t0)) { setView(t - span * 0.05, t + span * 0.95); }
    male(); requestAnimationFrame(lauf);
  };
  requestAnimationFrame(lauf);
}
function stop(behalten = false) {
  if (S.src) {
    if (behalten) S.offset = pos();
    S.src.onended = null;
    try { S.src.stop(); } catch (e) { /* schon gestoppt */ }
    S.src = null;
  }
  S.playing = false;
  $('#play').classList.remove('laeuft');
}
function springe(t) {
  if (S.playing) play(t); else { S.offset = Math.max(0, Math.min(S.dur, t)); male(); }
}

function lautstaerke() {
  const v = Number($('#lautstaerke').value) / 100;
  speicher.set('lautstaerke', String(Math.round(v * 100)));
  if (S.gain) S.gain.gain.setTargetAtTime(v * v, S.ctx.currentTime, 0.02);
}
$('#lautstaerke').value = speicher.get('lautstaerke', '80');
$('#lautstaerke').addEventListener('input', lautstaerke);

// ---------- Angleichen ----------
function zielLufs() {
  const v = $('#ziel').value;
  // automatisch: wie der Großteil, aber nicht extrem leise oder laut
  return v === 'median' ? Math.min(-9, Math.max(-18, S.ref)) : Number(v);
}
function verstaerkungen() {
  const ziel = zielLufs();
  return S.segs.map((s) => ({ a: s.a, b: s.b, db: s.lvl === 'leer' ? 0 : Math.max(-20, Math.min(20, ziel - s.lufs)) }));
}
function kuerzen() {
  if (!$('#trimmen').checked) return [0, S.dur];
  const a = S.info.lead > 0.3 ? S.info.lead - 0.1 : 0;
  const b = S.info.trail > 1.5 ? S.dur - (S.info.trail - 1) : S.dur;
  return [a, b];
}
async function angleichen() {
  const gains = verstaerkungen(), trim = kuerzen();
  const key = JSON.stringify([gains, trim]);
  if (S.levelled && S.levelKey === key) return S.levelled;
  korrekturStatus('laeuft', 'Korrektur wird berechnet …');
  fortschritt = (p) => korrekturStatus('laeuft', `Korrektur wird berechnet … ${Math.round(p * 100)} %`);
  const m = await frage({ cmd: 'level', gains, ceiling: -1, trim }, 'levelled');
  const buf = S.ctx.createBuffer(m.channels.length, m.channels[0].length, S.rate);
  m.channels.forEach((c, i) => buf.copyToChannel(c, i));
  S.levelled = buf; S.levelKey = key; S.levelTrim = trim[0];
  korrekturStatus('fertig');
  return buf;
}
function korrekturStatus(art, text) {
  const el = $('#k-status');
  $('#laden-wav').classList.toggle('hidden', art !== 'fertig');
  $('#korrigieren').textContent = art === 'fertig' ? 'Neu berechnen' : 'Automatisch korrigieren';
  if (!art) { el.classList.add('hidden'); return; }
  el.classList.remove('hidden');
  el.classList.toggle('laeuft', art === 'laeuft');
  if (art === 'fertig') {
    const teile = S.segs.filter((s) => s.lvl !== 'leer');
    const verzerrt = teile.filter((s) => s.clipEv >= 3 && s.rate > 2).length;
    const [a, b] = kuerzen();
    const weg = [];
    if (a > 0) weg.push(`${zahl(a)} s Stille am Anfang`);
    if (b < S.dur) weg.push(`${zahl(S.dur - b)} s am Ende`);
    text = `✓ Fertig: Alle ${teile.length} Teile sind jetzt gleich laut, die Spitzen werden begrenzt` +
      (weg.length ? `, ${weg.join(' und ')} gekürzt` : '') + '. ' +
      (verzerrt ? `${verzerrt === 1 ? 'Ein Teil bleibt' : verzerrt + ' Teile bleiben'} verzerrt – das lässt sich nicht automatisch reparieren. ` : '') +
      'Oben mit „Original | Korrigiert“ vergleichen.';
  }
  el.textContent = text;
}
async function modus(m, still = false, berechnen = false) {
  if (m === 'neu' && !S.levelled && !berechnen && !still) {
    // erst korrigieren, dann umschalten
    try { await angleichen(); } catch (e) { toast('Korrektur fehlgeschlagen: ' + e.message, 5000); return; }
  }
  if (berechnen && m === 'orig') { try { await angleichen(); } catch (e) { /* zeigt Knopf */ } }
  S.mode = m;
  S.dirty = true;
  $('#m-orig').classList.toggle('on', m === 'orig');
  $('#m-neu').classList.toggle('on', m === 'neu');
  if (m === 'neu') {
    try { await angleichen(); } catch (e) { toast('Korrektur fehlgeschlagen: ' + e.message, 5000); return; }
  }
  S.dirty = true; male();
  if (S.playing && !still) play(pos());
}

// ---------- Bedienung ----------
const naheSchnitt = (x, w, tol) => {
  let best = -1, bd = tol;
  S.cuts.forEach((c, i) => { const d = Math.abs(tx(c.f / FPS, w) - x); if (d <= bd) { bd = d; best = i; } });
  return best;
};
function setView(t0, t1) {
  const span = Math.max(2, Math.min(S.dur, t1 - t0));
  t0 = Math.max(0, Math.min(S.dur - span, t0));
  S.view = { t0, t1: t0 + span };
  S.dirty = true; male();
}
function zoom(f, um) {
  const span = S.view.t1 - S.view.t0;
  if (um == null) { const t = pos(); um = t >= S.view.t0 && t <= S.view.t1 ? t : (S.view.t0 + S.view.t1) / 2; }
  const ns = span * f, r = (um - S.view.t0) / span;
  setView(um - ns * r, um - ns * r + ns);
}
function waehle(i, zeigen = false) {
  S.sel = i;
  if (zeigen && S.ansicht === 'profi' && S.segs[i]) {
    const s = S.segs[i], pad = Math.max(1, (s.b - s.a) / FPS * 0.15);
    setView(s.a / FPS - pad, s.b / FPS + pad);
  }
  zeichneAlles();
}
function segAnFrame(f) {
  let lo = 0, hi = S.segs.length - 1;
  while (lo < hi) { const md = (lo + hi + 1) >> 1; if (S.segs[md].a <= f) lo = md; else hi = md - 1; }
  return lo;
}
function segBei(t) { const f = t * FPS; return S.segs.findIndex((s) => f >= s.a && f < s.b); }
function schnittSetzen(t) {
  const f = Math.round(t * FPS);
  if (f < 100 || f > S.nf - 100 || S.cuts.some((c) => Math.abs(c.f - f) < 100)) return toast('Zu nah an einem anderen Schnitt.');
  S.cuts.push({ f, why: 'hand' });
  neuBerechnen();
}
function schnittWeg(i) { S.cuts.splice(i, 1); if (S.sel >= S.cuts.length + 1) S.sel = -1; neuBerechnen(); }

let zug = null;
cvW.addEventListener('pointerdown', (e) => {
  if (!S.segs || e.button === 2) return;
  const r = cvW.getBoundingClientRect(), x = e.clientX - r.left;
  const tol = e.pointerType === 'touch' ? 16 : 7;
  const i = S.ansicht === 'profi' ? naheSchnitt(x, r.width, tol) : -1;
  zug = { x, i, t0: S.view.t0, t1: S.view.t1, moved: false, id: e.pointerId };
  if (i >= 0) { S.drag = i; S.dirty = true; male(); }
  cvW.setPointerCapture(e.pointerId);
});
cvW.addEventListener('pointermove', (e) => {
  const r = cvW.getBoundingClientRect(), x = e.clientX - r.left;
  if (!zug) {
    if (!S.segs) return;
    const h = S.ansicht === 'profi' ? naheSchnitt(x, r.width, 7) : -1;
    if (h !== S.hoverCut) { S.hoverCut = h; S.dirty = true; male(); }
    cvW.style.cursor = h >= 0 ? 'ew-resize' : 'crosshair';
    return;
  }
  if (Math.abs(x - zug.x) > 4) zug.moved = true;
  if (!zug.moved) return;
  if (zug.i >= 0) {
    const c = S.cuts[zug.i];
    const lo = (zug.i > 0 ? S.cuts[zug.i - 1].f : 0) + 100, hi = (zug.i + 1 < S.cuts.length ? S.cuts[zug.i + 1].f : S.nf) - 100;
    c.f = Math.max(lo, Math.min(hi, Math.round(xt(x, r.width) * FPS)));
    c.why = 'hand';
    S.segs[zug.i].b = c.f; S.segs[zug.i + 1].a = c.f;
    S.dirty = true; male();
  } else if (S.view.t1 - S.view.t0 < S.dur - 0.01) {
    const dt = (x - zug.x) / r.width * (zug.t1 - zug.t0);
    setView(zug.t0 - dt, zug.t1 - dt);
    cvW.style.cursor = 'grabbing';
  }
});
const loslassen = (e) => {
  if (!zug) return;
  const r = cvW.getBoundingClientRect(), x = e.clientX - r.left;
  const z = zug; zug = null; S.drag = -1;
  if (z.i >= 0 && z.moved) { neuBerechnen(); return; }
  if (!z.moved) {
    const t = xt(x, r.width);
    springe(t);
    const s = segBei(t);
    if (s !== S.sel) waehle(s);
  }
  S.dirty = true; male();
};
cvW.addEventListener('pointerup', loslassen);
cvW.addEventListener('pointercancel', () => { zug = null; S.drag = -1; });
cvW.addEventListener('dblclick', (e) => {
  if (S.ansicht !== 'profi') return; const r = cvW.getBoundingClientRect(); schnittSetzen(xt(e.clientX - r.left, r.width)); });
cvW.addEventListener('contextmenu', (e) => {
  if (S.ansicht !== 'profi') return;
  const r = cvW.getBoundingClientRect(), i = naheSchnitt(e.clientX - r.left, r.width, 9);
  if (i >= 0) { e.preventDefault(); schnittWeg(i); toast('Schnitt entfernt.'); }
});
cvW.addEventListener('pointerleave', () => { if (S.hoverCut >= 0) { S.hoverCut = -1; S.dirty = true; male(); } });
for (const cv of [cvW, cvK]) {
  cv.addEventListener('wheel', (e) => {
    if (!S.segs) return;
    const r = cv.getBoundingClientRect();
    if (e.ctrlKey || e.metaKey || e.altKey) {
      e.preventDefault();
      zoom(e.deltaY > 0 ? 1.25 : 0.8, xt(e.clientX - r.left, r.width));
    } else if (Math.abs(e.deltaX) > Math.abs(e.deltaY) || e.shiftKey) {
      e.preventDefault();
      const d = (e.shiftKey ? e.deltaY : e.deltaX) / r.width * (S.view.t1 - S.view.t0);
      setView(S.view.t0 + d, S.view.t1 + d);
    }
  }, { passive: false });
}
cvK.addEventListener('click', (e) => { const r = cvK.getBoundingClientRect(); springe(xt(e.clientX - r.left, r.width)); });

$('#play').addEventListener('click', () => (S.playing ? (stop(true), male()) : play()));
$('#m-orig').addEventListener('click', () => modus('orig'));
$('#m-neu').addEventListener('click', () => modus('neu'));
$('#z-ein').addEventListener('click', () => zoom(0.5));
$('#z-aus').addEventListener('click', () => zoom(2));
$('#z-alles').addEventListener('click', () => setView(0, S.dur));
$('#cut-add').addEventListener('click', () => schnittSetzen(pos()));
$('#cut-neu').addEventListener('click', () => { S.cuts = S.autoCuts.map((c) => ({ ...c })); S.sel = -1; neuBerechnen(); toast('Automatisch erkannte Schnitte wiederhergestellt.'); });
const optionGeaendert = () => { const war = !!S.levelled; S.levelled = null; zeichneAlles(); if (war || S.mode === 'neu') modus(S.mode, true, true).then(() => { if (S.playing) play(pos()); }); };
$('#ziel').addEventListener('change', optionGeaendert);
$('#trimmen').addEventListener('change', optionGeaendert);
$('#korrigieren').addEventListener('click', async (e) => {
  const btn = e.currentTarget; btn.disabled = true;
  S.levelled = null;
  try { await angleichen(); await modus('neu'); } catch (err) { korrekturStatus(null); toast('Korrektur fehlgeschlagen: ' + err.message, 5000); }
  btn.disabled = false;
});
$('#drucken').addEventListener('click', () => window.print());
$('#laden-wav').addEventListener('click', async (e) => {
  const btn = e.currentTarget;
  btn.disabled = true;
  try {
    const buf = await angleichen();
    const chs = [];
    for (let c = 0; c < buf.numberOfChannels; c++) chs.push(buf.getChannelData(c).slice());
    const m = await frage({ cmd: 'wav', channels: chs }, 'wav', chs.map((c) => c.buffer));
    const a = document.createElement('a');
    a.href = URL.createObjectURL(m.blob);
    a.download = S.file.name.replace(/\.[^.]+$/, '') + ' (korrigiert).wav';
    document.body.appendChild(a); a.click(); a.remove();
    setTimeout(() => URL.revokeObjectURL(a.href), 60000);
  } catch (err) { toast('Export fehlgeschlagen: ' + err.message, 5000); }
  btn.disabled = false;
});

document.addEventListener('click', (e) => {
  const t = e.target.closest('[data-play],[data-weg],[data-seg],[data-t]');
  if (!t) return;
  if (t.dataset.play != null) { const s = S.segs[+t.dataset.play]; waehle(+t.dataset.play); play(s.a / FPS); return; }
  if (t.dataset.weg != null) { schnittWeg(+t.dataset.weg); return; }
  if (t.dataset.t != null) { const tt = +t.dataset.t; setView(tt - 8, tt + 8); springe(tt - 3); return; }
  if (t.dataset.seg != null) { waehle(+t.dataset.seg, true); springe(S.segs[+t.dataset.seg].a / FPS); }
});
document.addEventListener('keydown', (e) => {
  if (e.code !== 'Space' || !S.segs || $('#ergebnis').classList.contains('hidden') || /INPUT|SELECT|BUTTON|TEXTAREA/.test(e.target.tagName)) return;
  e.preventDefault();
  S.playing ? (stop(true), male()) : play();
});
window.addEventListener('resize', () => { S.dirty = true; male(); });

// Datei wählen / ziehen
$('#datei').addEventListener('change', (e) => lade(e.target.files[0]));
const drop = $('#drop');
document.addEventListener('dragover', (e) => { e.preventDefault(); drop.classList.add('over'); });
document.addEventListener('dragleave', (e) => { if (!e.relatedTarget) drop.classList.remove('over'); });
document.addEventListener('drop', (e) => {
  e.preventDefault(); drop.classList.remove('over');
  const f = e.dataTransfer.files && e.dataTransfer.files[0];
  if (f) lade(f);
});
drop.addEventListener('keydown', (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); $('#datei').click(); } });
$('#neu').addEventListener('click', () => { stop(); S.segs = null; $('#datei').value = ''; zeige('start'); });
