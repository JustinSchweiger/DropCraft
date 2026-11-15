'use strict';
// Rechnet im Hintergrund, damit die Seite flüssig bleibt.
importScripts('kern.js');

let chs = null, rate = 48000, fr = null;

self.onmessage = (e) => {
  const m = e.data;
  try {
    if (m.cmd === 'analyse') {
      chs = m.channels; rate = m.rate;
      const res = analyse((p) => self.postMessage({ type: 'progress', p }));
      self.postMessage({ type: 'analysed', res });
    } else if (m.cmd === 'bpm') {
      const out = m.segs.map(([a, b]) => K.detectGrid(fr.low, fr.mid, fr.high, a, b));
      self.postMessage({ type: 'bpm', id: m.id, grids: out });
    } else if (m.cmd === 'level') {
      const out = level(m.gains, m.ceiling, (p) => self.postMessage({ type: 'progress', p }));
      self.postMessage({ type: 'levelled', id: m.id, channels: out }, out.map((c) => c.buffer));
    } else if (m.cmd === 'wav') {
      const blob = wav(m.channels, rate);
      self.postMessage({ type: 'wav', blob });
    }
  } catch (err) {
    self.postMessage({ type: 'error', message: String(err && err.stack || err) });
  }
};

function analyse(progress) {
  const C = chs.length, n = chs[0].length;
  const frameLen = rate * K.FRAME_MS / 1000;
  const nf = Math.max(1, Math.ceil(n / frameLen));
  // 1. Durchgang: Spitze der ganzen Datei (für die Übersteuerungs-Schwelle)
  let gpeak = 0;
  for (const ch of chs) for (let i = 0; i < n; i++) { const a = Math.abs(ch[i]); if (a > gpeak) gpeak = a; }
  const thr = Math.max(0.05, Math.min(gpeak * 0.995, 0.999));

  const low = new Float32Array(nf), mid = new Float32Array(nf), high = new Float32Array(nf);
  const peak = new Float32Array(nf), clip = new Uint16Array(nf), clipEv = new Uint16Array(nf);
  const z = new Float64Array(nf);
  const bands = K.bandFilters(rate);
  const kw = chs.map(() => K.kWeight(rate));
  const runLen = new Int32Array(C), runSign = new Int8Array(C);
  const sq = new Float64Array(C), dc = new Float64Array(C);
  let cross = 0, overs = 0;
  let f = 0, nextB = frameLen, cnt = 0;
  const bs = [0, 0, 0];
  let zs = 0, pk = 0;
  const step = Math.max(1, Math.floor(n / 50));
  for (let i = 0; i < n; i++) {
    let mono = 0;
    for (let c = 0; c < C; c++) {
      const x = chs[c][i];
      mono += x;
      sq[c] += x * x; dc[c] += x;
      const ax = x < 0 ? -x : x;
      if (ax > pk) pk = ax;
      if (ax > 1.0005) overs++;
      // Übersteuerung: ≥ 3 Samples in Folge an der Obergrenze
      if (ax >= thr) {
        const sg = x > 0 ? 1 : -1;
        if (runLen[c] > 0 && runSign[c] === sg) runLen[c]++; else { runLen[c] = 1; runSign[c] = sg; }
        if (runLen[c] === 3) { clipEv[f]++; clip[f] += 3; } else if (runLen[c] > 3 && clip[f] < 65000) clip[f]++;
      } else runLen[c] = 0;
      const k = kw[c];
      const y = K.run(k[1], K.run(k[0], x));
      zs += y * y;
    }
    if (C === 2) cross += chs[0][i] * chs[1][i];
    mono /= C;
    for (let b = 0; b < 3; b++) { const y = K.run(bands[b][1], K.run(bands[b][0], mono)); bs[b] += y * y; }
    cnt++;
    if (i + 1 >= nextB || i === n - 1) {
      low[f] = Math.sqrt(bs[0] / cnt); mid[f] = Math.sqrt(bs[1] / cnt); high[f] = Math.sqrt(bs[2] / cnt);
      z[f] = zs / cnt; peak[f] = pk;
      bs[0] = bs[1] = bs[2] = 0; zs = 0; pk = 0; cnt = 0;
      f = Math.min(nf - 1, f + 1);
      nextB = (f + 1) * frameLen;
    }
    if (i % step === 0) progress(0.05 + 0.6 * i / n);
  }
  // Bänder wie in DMXCraft auf die Datei normieren
  for (const v of [low, mid, high]) {
    const top = Math.max(K.percentile(v, 0.995), 1e-6);
    for (let i = 0; i < nf; i++) v[i] = Math.min(1, v[i] / top);
  }
  fr = { low, mid, high, peak, clip, clipEv, z };
  progress(0.7);
  const cuts = K.detectCuts(low, mid, high, z);
  progress(0.85);
  // Schnitt bei Tempowechsel auf den nächsten Taktanfang des neuen Teils legen
  const bounds = [0, ...cuts.map((c) => c.f), nf];
  const grids = [];
  for (let s = 0; s + 1 < bounds.length; s++) grids.push(K.detectGrid(low, mid, high, bounds[s], bounds[s + 1]));
  for (let s = 1; s < grids.length; s++) {
    const g = grids[s], c = cuts[s - 1];
    if (!g || c.why !== 'tempo' || c.exact) continue;
    const start = bounds[s] * K.FRAME_MS, bar = 4 * 60000 / g.bpm;
    const near = g.downbeat + Math.round((start - g.downbeat) / bar) * bar;
    if (Math.abs(near - start) <= bar / 2 && near > bounds[s - 1] * K.FRAME_MS + 1000 && near < bounds[s + 1] * K.FRAME_MS - 1000) {
      c.f = Math.round(near / K.FRAME_MS);
    }
  }
  progress(0.97);
  // Stille am Anfang/Ende (unter −50 dBFS)
  const silent = 0.00316;
  let lead = 0; while (lead < nf && peak[lead] < silent) lead++;
  let trail = 0; while (trail < nf - lead && peak[nf - 1 - trail] < silent) trail++;
  const corr = C === 2 && sq[0] > 0 && sq[1] > 0 ? cross / Math.sqrt(sq[0] * sq[1]) : null;
  return {
    nf, frames: { low, mid, high, peak, clip, clipEv, z },
    cuts, grids,
    info: {
      peak: K.dbfs(gpeak), overs, channels: C,
      rms: Array.from(sq, (s) => K.dbfs(Math.sqrt(s / n))),
      dc: Array.from(dc, (s) => s / n),
      corr, lead: lead * K.FRAME_MS / 1000, trail: trail * K.FRAME_MS / 1000,
    },
  };
}

// Angleichen: Verstärkung je Teil (weich übergeblendet) und Limiter
function level(gains, ceilingDb, progress) {
  const C = chs.length, n = chs[0].length;
  const frameLen = rate * K.FRAME_MS / 1000;
  const g = new Float32Array(n);
  const ramp = Math.round(rate * 0.04);
  // Stufen setzen
  for (const s of gains) {
    const a = Math.round(s.a * frameLen), b = Math.min(n, Math.round(s.b * frameLen));
    g.fill(Math.pow(10, s.db / 20), a, b);
  }
  // Übergänge weich machen (40 ms)
  for (let k = 1; k < gains.length; k++) {
    const at = Math.round(gains[k].a * frameLen);
    const g0 = Math.pow(10, gains[k - 1].db / 20), g1 = Math.pow(10, gains[k].db / 20);
    for (let i = -ramp; i < ramp; i++) {
      const j = at + i;
      if (j < 0 || j >= n) continue;
      const t = (i + ramp) / (2 * ramp);
      g[j] = g0 + (g1 - g0) * (0.5 - 0.5 * Math.cos(Math.PI * t));
    }
  }
  progress(0.1);
  // Limiter mit 5 ms Vorausschau
  const ceil = Math.pow(10, ceilingDb / 20);
  const L = Math.max(1, Math.round(rate * 0.005));
  const need = new Float32Array(n);
  for (let i = 0; i < n; i++) {
    let m = 0;
    for (let c = 0; c < C; c++) { const v = Math.abs(chs[c][i] * g[i]); if (v > m) m = v; }
    need[i] = m > ceil ? ceil / m : 1;
  }
  progress(0.3);
  // gleitendes Minimum über [i, i+L)
  const mn = new Float32Array(n);
  const dq = new Int32Array(n); let h = 0, t = 0;
  for (let i = n - 1; i >= 0; i--) {
    while (t > h && need[dq[t - 1]] >= need[i]) t--;
    dq[t++] = i;
    while (dq[h] >= i + L) h++;
    mn[i] = need[dq[h]];
  }
  progress(0.5);
  // Mittelwert über [i-L+1, i] (sanfter Einsatz) + langsames Loslassen
  const rel = 1 - Math.exp(-1 / (rate * 0.08));
  let acc = 0, prev = 1;
  const out = chs.map(() => new Float32Array(n));
  for (let i = 0; i < n; i++) {
    acc += mn[i]; if (i >= L) acc -= mn[i - L];
    const s = acc / Math.min(L, i + 1);
    const lim = s < prev ? s : prev + (s - prev) * rel;
    prev = Math.min(lim, s);
    const gg = g[i] * prev;
    for (let c = 0; c < C; c++) {
      let v = chs[c][i] * gg;
      if (v > ceil) v = ceil; else if (v < -ceil) v = -ceil;
      out[c][i] = v;
    }
  }
  progress(1);
  return out;
}

function wav(channels, sr) {
  const C = channels.length, n = channels[0].length;
  const buf = new ArrayBuffer(44 + n * C * 2), dv = new DataView(buf);
  const str = (o, s) => { for (let i = 0; i < s.length; i++) dv.setUint8(o + i, s.charCodeAt(i)); };
  str(0, 'RIFF'); dv.setUint32(4, 36 + n * C * 2, true); str(8, 'WAVE');
  str(12, 'fmt '); dv.setUint32(16, 16, true); dv.setUint16(20, 1, true); dv.setUint16(22, C, true);
  dv.setUint32(24, sr, true); dv.setUint32(28, sr * C * 2, true); dv.setUint16(32, C * 2, true); dv.setUint16(34, 16, true);
  str(36, 'data'); dv.setUint32(40, n * C * 2, true);
  let o = 44;
  for (let i = 0; i < n; i++) {
    for (let c = 0; c < C; c++) {
      const d = (Math.random() - Math.random()) / 32768; // leichtes Dither
      const v = Math.max(-1, Math.min(1, channels[c][i] + d));
      dv.setInt16(o, v < 0 ? v * 32768 : v * 32767, true); o += 2;
    }
  }
  return new Blob([buf], { type: 'audio/wav' });
}
