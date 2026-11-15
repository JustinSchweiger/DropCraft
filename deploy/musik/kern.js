'use strict';
// Rechenkern des Musik-Checks – wird von der Seite und vom Worker geladen.
// Tempo- und Schnitterkennung nach dem Vorbild von DMXCraft (core/audio.rs).

const K = (() => {
  const FRAME_MS = 10;
  const LOW_HZ = 150, HIGH_HZ = 2500;
  const BPM_MIN = 70, BPM_MAX = 180;
  const SEG_WIN = 1000, SEG_HOP = 200, SEG_MIN = 800;
  const TAU = Math.PI * 2;

  // ---------- Filter ----------
  function biquad(kind, hz, q, rate, gainDb) {
    const w = TAU * Math.min(hz / rate, 0.49);
    const s = Math.sin(w), c = Math.cos(w), al = s / (2 * q);
    let b, a0, a;
    if (kind === 'low') { b = [(1 - c) / 2, 1 - c, (1 - c) / 2]; a0 = 1 + al; a = [-2 * c, 1 - al]; }
    else if (kind === 'high') { b = [(1 + c) / 2, -(1 + c), (1 + c) / 2]; a0 = 1 + al; a = [-2 * c, 1 - al]; }
    else if (kind === 'band') { b = [al, 0, -al]; a0 = 1 + al; a = [-2 * c, 1 - al]; }
    else { // Höhenanhebung (Shelf) der K-Bewertung
      const A = Math.pow(10, gainDb / 40), sq = 2 * Math.sqrt(A) * al;
      b = [A * ((A + 1) + (A - 1) * c + sq), -2 * A * ((A - 1) + (A + 1) * c), A * ((A + 1) + (A - 1) * c - sq)];
      a0 = (A + 1) - (A - 1) * c + sq;
      a = [2 * ((A - 1) - (A + 1) * c), (A + 1) - (A - 1) * c - sq];
    }
    return { b0: b[0] / a0, b1: b[1] / a0, b2: b[2] / a0, a1: a[0] / a0, a2: a[1] / a0, x1: 0, x2: 0, y1: 0, y2: 0 };
  }
  function run(f, x) {
    const y = f.b0 * x + f.b1 * f.x1 + f.b2 * f.x2 - f.a1 * f.y1 - f.a2 * f.y2;
    f.x2 = f.x1; f.x1 = x; f.y2 = f.y1; f.y1 = y;
    return y;
  }
  // K-Bewertung nach ITU-R BS.1770 (Shelf + Hochpass)
  const kWeight = (rate) => [biquad('shelf', 1500, Math.SQRT1_2, rate, 4), biquad('high', 38, 0.5, rate)];
  const bandFilters = (rate) => {
    const center = Math.sqrt(LOW_HZ * HIGH_HZ);
    return [
      [biquad('low', LOW_HZ, 0.707, rate), biquad('low', LOW_HZ, 0.707, rate)],
      [biquad('band', center, 0.5, rate), biquad('band', center, 0.5, rate)],
      [biquad('high', HIGH_HZ, 0.707, rate), biquad('high', HIGH_HZ, 0.707, rate)],
    ];
  };

  // ---------- Hilfen ----------
  function percentile(v, p) {
    if (!v.length) return 0;
    const s = Float32Array.from(v).sort();
    return s[Math.floor((s.length - 1) * p)];
  }
  function smooth(v, r) {
    const n = v.length, pre = new Float64Array(n + 1);
    for (let i = 0; i < n; i++) pre[i + 1] = pre[i] + v[i];
    const out = new Float32Array(n);
    for (let i = 0; i < n; i++) {
      const a = Math.max(0, i - r), b = Math.min(n, i + r + 1);
      out[i] = (pre[b] - pre[a]) / (b - a);
    }
    return out;
  }
  function onsets(v) {
    const n = v.length, d = new Float32Array(n);
    let prev = Math.log(1 + 100 * (v[0] || 0));
    for (let i = 1; i < n; i++) {
      const l = Math.log(1 + 100 * v[i]);
      d[i] = Math.max(0, l - prev);
      prev = l;
    }
    const m = smooth(d, 25);
    for (let i = 0; i < n; i++) d[i] = Math.max(0, d[i] - m[i]);
    return d;
  }
  function tempoCoef(o, bpm) {
    const w = TAU * bpm / 60 * FRAME_MS / 1000;
    const sw = Math.sin(-w), cw = Math.cos(-w);
    let re = 0, im = 0, pr = 1, pi = 0;
    for (let n = 0; n < o.length; n++) {
      const x = o[n];
      re += x * pr; im += x * pi;
      const t = pr * cw - pi * sw; pi = pr * sw + pi * cw; pr = t;
      if ((n & 1023) === 1023) { const m = Math.hypot(pr, pi); pr /= m; pi /= m; }
    }
    return [Math.hypot(re, im), Math.atan2(im, re)];
  }
  const tempoScore = (o, bpm) => tempoCoef(o, bpm)[0] + 0.5 * tempoCoef(o, bpm * 2)[0];
  const sum = (v) => { let s = 0; for (let i = 0; i < v.length; i++) s += v[i]; return s; };
  function localTempo(o) {
    const total = sum(o);
    if (total < 1e-3) return null;
    let best = [0, 0];
    for (let b = BPM_MIN; b <= BPM_MAX; b += 0.5) {
      const sc = tempoScore(o, b);
      if (sc > best[1]) best = [b, sc];
    }
    return tempoCoef(o, best[0])[0] / total >= 0.08 ? best[0] : null;
  }
  const sameTempo = (a, b) => [1, 2, 0.5].some((k) => Math.abs(a * k / b - 1) < 0.02);

  // Tempo und Taktanfang eines Abschnitts (Frames a..b)
  function detectGrid(low, mid, high, a, b) {
    const L = low.subarray(a, b), M = mid.subarray(a, b), H = high.subarray(a, b);
    const n = L.length;
    if (n < 400) return null;
    const ol = onsets(L), om = onsets(M), oh = onsets(H);
    const o = new Float32Array(n);
    for (let i = 0; i < n; i++) o[i] = ol[i] + 0.6 * om[i] + 0.6 * oh[i];
    const total = sum(o);
    if (total < 1e-3) return null;
    let best = [BPM_MIN, 0];
    for (let bb = BPM_MIN; bb <= BPM_MAX; bb += 0.1) {
      const s = tempoScore(o, bb);
      if (s > best[1]) best = [bb, s];
    }
    let fine = best;
    for (let bb = best[0] - 0.1; bb <= best[0] + 0.1; bb += 0.005) {
      const s = tempoScore(o, bb);
      if (s > fine[1]) fine = [bb, s];
    }
    const bpm = fine[0];
    if (tempoCoef(o, bpm)[0] / total < 0.05) return null;
    const period = 60000 / bpm / FRAME_MS;
    const on = new Float32Array(n);
    for (let i = 0; i < n; i++) on[i] = ol[i] + 0.5 * om[i] - 0.3 * oh[i];
    const kernel = (v, f) => {
      const lo = Math.max(0, Math.ceil(f - 1.5)), hi = Math.min(n - 1, Math.floor(f + 1.5));
      let s = 0;
      for (let i = lo; i <= hi; i++) s += v[i] * Math.max(0, 1 - Math.abs(i - f) / 1.5);
      return s;
    };
    let phase = [0, -Infinity];
    for (let ph = 0; ph < period; ph += 0.25) {
      let s = 0;
      for (let f = ph; f < n; f += period) s += kernel(on, f);
      if (s > phase[1]) phase = [ph, s];
    }
    const at = (v, f) => { const i = Math.round(f); let m = 0; for (let k = i - 1; k <= i + 1; k++) if (k >= 0 && k < n) m = Math.max(m, v[k]); return m; };
    const lev = new Float32Array(n);
    for (let i = 0; i < n; i++) lev[i] = L[i] + M[i] + H[i];
    const mean = (x, y) => {
      x = Math.max(0, Math.floor(x)); y = Math.min(n, Math.floor(Math.max(0, y)));
      if (y <= x) return 0;
      let s = 0; for (let i = x; i < y; i++) s += lev[i]; return s / (y - x);
    };
    const score = [0, 0, 0, 0], count = [0, 0, 0, 0];
    for (let k = 0; ; k++) {
      const f = phase[0] + k * period;
      if (f >= n) break;
      const bar = period * 4, j = k % 4;
      score[j] += at(ol, f) + 0.5 * at(oh, f) + 2 * Math.max(0, mean(f, f + bar) - mean(f - bar, f));
      count[j]++;
    }
    let j = 0;
    for (let q = 1; q < 4; q++) if (score[q] / Math.max(1, count[q]) > score[j] / Math.max(1, count[j])) j = q;
    const barMs = 4 * 60000 / bpm;
    let down = ((phase[0] + j * period) * FRAME_MS) % barMs;
    return { bpm: Math.round(bpm * 100) / 100, downbeat: down + a * FRAME_MS };
  }

  // ---------- Lautheit ----------
  const lufs = (ms) => (ms > 0 ? -0.691 + 10 * Math.log10(ms) : -Infinity);
  const dbfs = (lin) => (lin > 0 ? 20 * Math.log10(lin) : -Infinity);
  // integrierte Lautheit mit Gate (−70 LUFS absolut, −10 LU relativ)
  function integrated(z, a, b) {
    const blocks = [];
    for (let i = a; i + 40 <= b; i += 10) {
      let s = 0;
      for (let k = i; k < i + 40; k++) s += z[k];
      blocks.push(s / 40);
    }
    if (!blocks.length) {
      let s = 0; for (let k = a; k < b; k++) s += z[k];
      return b > a ? lufs(s / (b - a)) : -Infinity;
    }
    const absT = Math.pow(10, (-70 + 0.691) / 10);
    let s1 = 0, n1 = 0;
    for (const m of blocks) if (m > absT) { s1 += m; n1++; }
    if (!n1) return -Infinity;
    const relT = Math.pow(10, (lufs(s1 / n1) - 10 + 0.691) / 10);
    let s2 = 0, n2 = 0;
    for (const m of blocks) if (m > absT && m > relT) { s2 += m; n2++; }
    return lufs(s2 / n2);
  }
  const prefix = (v) => { const p = new Float64Array(v.length + 1); for (let i = 0; i < v.length; i++) p[i + 1] = p[i] + v[i]; return p; };
  const meanDb = (p, a, b) => { a = Math.max(0, a); b = Math.min(p.length - 1, b); return b > a ? lufs((p[b] - p[a]) / (b - a)) : -Infinity; };

  // Kurzzeit-Lautheit (3 s) alle 100 ms
  function shortTerm(z) {
    const p = prefix(z), out = new Float32Array(Math.ceil(z.length / 10));
    for (let j = 0; j < out.length; j++) out[j] = Math.max(-80, meanDb(p, j * 10 - 150, j * 10 + 150));
    return out;
  }

  // ---------- Schnitte finden ----------
  // Tempowechsel (wie DMXCraft), Pausen und Lautstärkesprünge
  function detectCuts(low, mid, high, z) {
    const n = low.length;
    if (n < SEG_MIN * 2) return [];
    const ol = onsets(low), om = onsets(mid), oh = onsets(high);
    const o = new Float32Array(n);
    for (let i = 0; i < n; i++) o[i] = ol[i] + 0.6 * om[i] + 0.6 * oh[i];
    const local = [];
    for (let a = 0; a + SEG_WIN <= n; a += SEG_HOP) local.push([a + SEG_WIN / 2, localTempo(o.subarray(a, a + SEG_WIN))]);
    const pre = [prefix(low), prefix(mid), prefix(high)];
    const novelty = (i) => {
      const w = Math.min(400, i, n - i);
      if (w < 50) return 0;
      let s = 0;
      for (const p of pre) s += Math.abs((p[i] - p[i - w]) / w - (p[i + w] - p[i]) / w);
      return s;
    };
    const cuts = [];
    let cur = null;
    for (let k = 0; k < local.length; k++) {
      const [center, bpm] = local[k];
      if (cur === null && bpm !== null) cur = bpm;
      else if (cur !== null && bpm !== null && !sameTempo(cur, bpm)) {
        const next = local[k + 1] && local[k + 1][1];
        if (next && sameTempo(bpm, next)) {
          const from = Math.max(0, local[Math.max(0, k - 1)][0] - SEG_WIN / 2);
          const to = Math.min(n - 1, center + SEG_WIN / 2);
          let best = center, bs = -1;
          for (let x = from; x <= to; x += 5) { const v = novelty(x); if (v > bs) { bs = v; best = x; } }
          cuts.push({ f: best, why: 'tempo', info: [Math.round(cur), Math.round(bpm)] });
          cur = bpm;
        }
      }
    }
    // Pausen (≥ 0,3 s fast still)
    let quiet = 0;
    for (let i = 0; i < n; i++) {
      if (low[i] + mid[i] + high[i] < 0.03) quiet++;
      else { if (quiet >= 30 && i > quiet) cuts.push({ f: i, why: 'pause', info: quiet * FRAME_MS }); quiet = 0; }
    }
    // Lautstärkesprünge: 4 s davor/danach ≥ 4 LU und auch kurzfristig deutlich
    const zp = prefix(z);
    const jumps = [];
    for (let i = 400; i <= n - 400; i += 10) {
      const d = meanDb(zp, i, i + 400) - meanDb(zp, i - 400, i);
      const d1 = meanDb(zp, i, i + 100) - meanDb(zp, i - 100, i);
      if (Math.abs(d) >= 4 && Math.abs(d1) >= 3 && Math.sign(d) === Math.sign(d1) && isFinite(d)) jumps.push([i, d]);
    }
    for (const [i, d] of jumps) {
      if (jumps.some(([j, e]) => Math.abs(j - i) <= 300 && Math.abs(e) > Math.abs(d))) continue;
      let best = i, bs = -1;
      for (let j = i - 100; j <= i + 100; j++) {
        const v = Math.abs(meanDb(zp, j, j + 50) - meanDb(zp, j - 50, j));
        if (isFinite(v) && v > bs) { bs = v; best = j; }
      }
      cuts.push({ f: best, why: 'pegel', info: Math.round(d * 10) / 10 });
    }
    cuts.sort((a, b) => a.f - b.f);
    // nahe beieinander: zusammenfassen (Tempo/Pause gewinnt), zu kurze Teile vermeiden
    const merged = [];
    for (const c of cuts) {
      const last = merged[merged.length - 1];
      if (last && c.f - last.f < 300) {
        // Grund: Tempo vor Pause vor Pegel; Position: Pause/Pegel sind genauer als das Tempo
        const rang = { tempo: 0, pause: 1, pegel: 2 };
        const [haupt, neben] = rang[c.why] < rang[last.why] ? [c, last] : [last, c];
        const genau = [c, last].filter((x) => x.why !== 'tempo').sort((x, y) => rang[x.why] - rang[y.why])[0];
        const m = { ...haupt, also: neben };
        if (genau) { m.f = genau.f; m.exact = true; }
        merged[merged.length - 1] = m;
        continue;
      }
      merged.push(c);
    }
    const out = [];
    let lastF = 0;
    for (const c of merged) {
      if (c.f >= lastF + SEG_MIN && c.f + SEG_MIN <= n) { out.push(c); lastF = c.f; }
    }
    return out;
  }

  // Segmentwerte (Frames a..b)
  function segStats(fr, a, b) {
    let peak = 0, clipEv = 0, clipS = 0;
    for (let i = a; i < b; i++) {
      if (fr.peak[i] > peak) peak = fr.peak[i];
      clipEv += fr.clipEv[i]; clipS += fr.clip[i];
    }
    let stMax = -Infinity;
    for (let j = Math.ceil(a / 10); j < Math.floor(b / 10); j++) if (fr.st[j] > stMax) stMax = fr.st[j];
    return { a, b, dur: (b - a) * FRAME_MS / 1000, lufs: integrated(fr.z, a, b), stMax, peak: dbfs(peak), clipEv, clipS };
  }

  return { FRAME_MS, biquad, run, kWeight, bandFilters, percentile, detectGrid, detectCuts, integrated, shortTerm, segStats, lufs, dbfs, prefix, meanDb, sameTempo };
})();
if (typeof self !== 'undefined') self.K = K;
