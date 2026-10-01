// zenoh-web browser client, bundled from github.com/jeff-hykin/zenoh-web@8d3825101d4887f6c9eefd2c2ade211a390526ed by run/vendor_zenoh_web. Do not edit.
// zenoh-web/client/vendor/fzstd.ts
var ab = ArrayBuffer;
var u8 = Uint8Array;
var u16 = Uint16Array;
var i16 = Int16Array;
var i32 = Int32Array;
var slc = (v, s, e) => {
  if (u8.prototype.slice) return u8.prototype.slice.call(v, s, e);
  if (s == null || s < 0) s = 0;
  if (e == null || e > v.length) e = v.length;
  const n = new u8(e - s);
  n.set(v.subarray(s, e));
  return n;
};
var fill = (v, n, s, e) => {
  if (u8.prototype.fill) return u8.prototype.fill.call(v, n, s, e);
  if (s == null || s < 0) s = 0;
  if (e == null || e > v.length) e = v.length;
  for (; s < e; ++s) v[s] = n;
  return v;
};
var cpw = (v, t, s, e) => {
  if (u8.prototype.copyWithin) return u8.prototype.copyWithin.call(v, t, s, e);
  if (s == null || s < 0) s = 0;
  if (e == null || e > v.length) e = v.length;
  while (s < e) {
    v[t++] = v[s++];
  }
};
var ec = [
  "invalid zstd data",
  "window size too large (>2046MB)",
  "invalid block type",
  "FSE accuracy too high",
  "match distance too far back",
  "unexpected EOF"
];
var err = (ind, msg, nt) => {
  const e = new Error(msg || ec[ind]);
  e.code = ind;
  if (Error.captureStackTrace) Error.captureStackTrace(e, err);
  if (!nt) throw e;
  return e;
};
var rb = (d, b, n) => {
  let i = 0, o = 0;
  for (; i < n; ++i) o |= d[b++] << (i << 3);
  return o;
};
var b4 = (d, b) => (d[b] | d[b + 1] << 8 | d[b + 2] << 16 | d[b + 3] << 24) >>> 0;
var rzfh = (dat, w) => {
  const n3 = dat[0] | dat[1] << 8 | dat[2] << 16;
  if (n3 == 3126568 && dat[3] == 253) {
    const flg = dat[4];
    const ss = flg >> 5 & 1, cc = flg >> 2 & 1, df = flg & 3, fcf = flg >> 6;
    if (flg & 8) err(0);
    let bt = 6 - ss;
    const db = df == 3 ? 4 : df;
    const di = rb(dat, bt, db);
    bt += db;
    const fsb = fcf ? 1 << fcf : ss;
    const fss = rb(dat, bt, fsb) + (fcf == 1 && 256);
    let ws = fss;
    if (!ss) {
      const wb = 1 << 10 + (dat[5] >> 3);
      ws = wb + (wb >> 3) * (dat[5] & 7);
    }
    if (ws > 2145386496) err(1);
    const buf = new u8((w == 1 ? fss || ws : w ? 0 : ws) + 12);
    buf[0] = 1, buf[4] = 4, buf[8] = 8;
    return {
      b: bt + fsb,
      y: 0,
      l: 0,
      d: di,
      w: w && w != 1 ? w : buf.subarray(12),
      e: ws,
      o: new i32(buf.buffer, 0, 3),
      u: fss,
      c: cc,
      m: Math.min(131072, ws)
    };
  } else if ((n3 >> 4 | dat[3] << 20) == 25481893) {
    return b4(dat, 4) + 8;
  }
  err(0);
};
var msb = (val) => {
  let bits = 0;
  for (; 1 << bits <= val; ++bits) ;
  return bits - 1;
};
var rfse = (dat, bt, mal) => {
  let tpos = (bt << 3) + 4;
  const al = (dat[bt] & 15) + 5;
  if (al > mal) err(3);
  const sz = 1 << al;
  let probs = sz, sym = -1, re = -1, i = -1, ht = sz;
  const buf = new ab(512 + (sz << 2));
  const freq = new i16(buf, 0, 256);
  const dstate = new u16(buf, 0, 256);
  const nstate = new u16(buf, 512, sz);
  const bb1 = 512 + (sz << 1);
  const syms = new u8(buf, bb1, sz);
  const nbits = new u8(buf, bb1 + sz);
  while (sym < 255 && probs > 0) {
    const bits = msb(probs + 1);
    const cbt = tpos >> 3;
    const msk = (1 << bits + 1) - 1;
    let val = (dat[cbt] | dat[cbt + 1] << 8 | dat[cbt + 2] << 16) >> (tpos & 7) & msk;
    const msk1fb = (1 << bits) - 1;
    const msv = msk - probs - 1;
    const sval = val & msk1fb;
    if (sval < msv) tpos += bits, val = sval;
    else {
      tpos += bits + 1;
      if (val > msk1fb) val -= msv;
    }
    freq[++sym] = --val;
    if (val == -1) {
      probs += val;
      syms[--ht] = sym;
    } else probs -= val;
    if (!val) {
      do {
        const rbt = tpos >> 3;
        re = (dat[rbt] | dat[rbt + 1] << 8) >> (tpos & 7) & 3;
        tpos += 2;
        sym += re;
      } while (re == 3);
    }
  }
  if (sym > 255 || probs) err(0);
  let sympos = 0;
  const sstep = (sz >> 1) + (sz >> 3) + 3;
  const smask = sz - 1;
  for (let s = 0; s <= sym; ++s) {
    const sf = freq[s];
    if (sf < 1) {
      dstate[s] = -sf;
      continue;
    }
    for (i = 0; i < sf; ++i) {
      syms[sympos] = s;
      do {
        sympos = sympos + sstep & smask;
      } while (sympos >= ht);
    }
  }
  if (sympos) err(0);
  for (i = 0; i < sz; ++i) {
    const ns = dstate[syms[i]]++;
    const nb = nbits[i] = al - msb(ns);
    nstate[i] = (ns << nb) - sz;
  }
  return [
    tpos + 7 >> 3,
    {
      b: al,
      s: syms,
      n: nbits,
      t: nstate
    }
  ];
};
var rhu = (dat, bt) => {
  let i = 0, wc = -1;
  const buf = new u8(292), hb = dat[bt];
  const hw = buf.subarray(0, 256);
  const rc = buf.subarray(256, 268);
  const ri = new u16(buf.buffer, 268);
  if (hb < 128) {
    const [ebt, fdt] = rfse(dat, bt + 1, 6);
    bt += hb;
    const epos = ebt << 3;
    const lb = dat[bt];
    if (!lb) err(0);
    let st1 = 0, st2 = 0, btr1 = fdt.b, btr2 = btr1;
    let fpos = (++bt << 3) - 8 + msb(lb);
    for (; ; ) {
      fpos -= btr1;
      if (fpos < epos) break;
      let cbt = fpos >> 3;
      st1 += (dat[cbt] | dat[cbt + 1] << 8) >> (fpos & 7) & (1 << btr1) - 1;
      hw[++wc] = fdt.s[st1];
      fpos -= btr2;
      if (fpos < epos) break;
      cbt = fpos >> 3;
      st2 += (dat[cbt] | dat[cbt + 1] << 8) >> (fpos & 7) & (1 << btr2) - 1;
      hw[++wc] = fdt.s[st2];
      btr1 = fdt.n[st1];
      st1 = fdt.t[st1];
      btr2 = fdt.n[st2];
      st2 = fdt.t[st2];
    }
    if (++wc > 255) err(0);
  } else {
    wc = hb - 127;
    for (; i < wc; i += 2) {
      const byte = dat[++bt];
      hw[i] = byte >> 4;
      hw[i + 1] = byte & 15;
    }
    ++bt;
  }
  let wes = 0;
  for (i = 0; i < wc; ++i) {
    const wt = hw[i];
    if (wt > 11) err(0);
    wes += wt && 1 << wt - 1;
  }
  const mb = msb(wes) + 1;
  const ts = 1 << mb;
  const rem = ts - wes;
  if (rem & rem - 1) err(0);
  hw[wc++] = msb(rem) + 1;
  for (i = 0; i < wc; ++i) {
    const wt = hw[i];
    ++rc[hw[i] = wt && mb + 1 - wt];
  }
  const hbuf = new u8(ts << 1);
  const syms = hbuf.subarray(0, ts), nb = hbuf.subarray(ts);
  ri[mb] = 0;
  for (i = mb; i > 0; --i) {
    const pv = ri[i];
    fill(nb, i, pv, ri[i - 1] = pv + rc[i] * (1 << mb - i));
  }
  if (ri[0] != ts) err(0);
  for (i = 0; i < wc; ++i) {
    const bits = hw[i];
    if (bits) {
      const code = ri[bits];
      fill(syms, i, code, ri[bits] = code + (1 << mb - bits));
    }
  }
  return [
    bt,
    {
      n: nb,
      b: mb,
      s: syms
    }
  ];
};
var dllt = rfse(/* @__PURE__ */ new u8([
  81,
  16,
  99,
  140,
  49,
  198,
  24,
  99,
  12,
  33,
  196,
  24,
  99,
  102,
  102,
  134,
  70,
  146,
  4
]), 0, 6)[1];
var dmlt = rfse(/* @__PURE__ */ new u8([
  33,
  20,
  196,
  24,
  99,
  140,
  33,
  132,
  16,
  66,
  8,
  33,
  132,
  16,
  66,
  8,
  33,
  68,
  68,
  68,
  68,
  68,
  68,
  68,
  68,
  36,
  9
]), 0, 6)[1];
var doct = rfse(/* @__PURE__ */ new u8([
  32,
  132,
  16,
  66,
  102,
  70,
  68,
  68,
  68,
  68,
  36,
  73,
  2
]), 0, 5)[1];
var b2bl = (b, s) => {
  const len = b.length, bl = new i32(len);
  for (let i = 0; i < len; ++i) {
    bl[i] = s;
    s += 1 << b[i];
  }
  return bl;
};
var llb = /* @__PURE__ */ new u8(new i32([
  0,
  0,
  0,
  0,
  16843009,
  50528770,
  134678020,
  202050057,
  269422093
]).buffer, 0, 36);
var llbl = /* @__PURE__ */ b2bl(llb, 0);
var mlb = /* @__PURE__ */ new u8(new i32([
  0,
  0,
  0,
  0,
  0,
  0,
  0,
  0,
  16843009,
  50528770,
  117769220,
  185207048,
  252579084,
  16
]).buffer, 0, 53);
var mlbl = /* @__PURE__ */ b2bl(mlb, 3);
var dhu = (dat, out, hu) => {
  const len = dat.length, ss = out.length, lb = dat[len - 1], msk = (1 << hu.b) - 1, eb = -hu.b;
  if (!lb) err(0);
  let st = 0, btr = hu.b, pos = (len << 3) - 8 + msb(lb) - btr, i = -1;
  for (; pos > eb && i < ss; ) {
    const cbt = pos >> 3;
    const val = (dat[cbt] | dat[cbt + 1] << 8 | dat[cbt + 2] << 16) >> (pos & 7);
    st = (st << btr | val) & msk;
    out[++i] = hu.s[st];
    pos -= btr = hu.n[st];
  }
  if (pos != eb || i + 1 != ss) err(0);
};
var dhu4 = (dat, out, hu) => {
  let bt = 6;
  const ss = out.length, sz1 = ss + 3 >> 2, sz2 = sz1 << 1, sz3 = sz1 + sz2;
  dhu(dat.subarray(bt, bt += dat[0] | dat[1] << 8), out.subarray(0, sz1), hu);
  dhu(dat.subarray(bt, bt += dat[2] | dat[3] << 8), out.subarray(sz1, sz2), hu);
  dhu(dat.subarray(bt, bt += dat[4] | dat[5] << 8), out.subarray(sz2, sz3), hu);
  dhu(dat.subarray(bt), out.subarray(sz3), hu);
};
var rzb = (dat, st, out) => {
  let bt = st.b;
  const b0 = dat[bt], btype = b0 >> 1 & 3;
  st.l = b0 & 1;
  const sz = b0 >> 3 | dat[bt + 1] << 5 | dat[bt + 2] << 13;
  const ebt = (bt += 3) + sz;
  if (btype == 1) {
    if (bt >= dat.length) return;
    st.b = bt + 1;
    if (out) {
      fill(out, dat[bt], st.y, st.y += sz);
      return out;
    }
    return fill(new u8(sz), dat[bt]);
  }
  if (ebt > dat.length) return;
  if (btype == 0) {
    st.b = ebt;
    if (out) {
      out.set(dat.subarray(bt, ebt), st.y);
      st.y += sz;
      return out;
    }
    return slc(dat, bt, ebt);
  }
  if (btype == 2) {
    const b3 = dat[bt], lbt = b3 & 3, sf = b3 >> 2 & 3;
    let lss = b3 >> 4, lcs = 0, s4 = 0;
    if (lbt < 2) {
      if (sf & 1) lss |= dat[++bt] << 4 | (sf & 2 && dat[++bt] << 12);
      else lss = b3 >> 3;
    } else {
      s4 = sf;
      if (sf < 2) lss |= (dat[++bt] & 63) << 4, lcs = dat[bt] >> 6 | dat[++bt] << 2;
      else if (sf == 2) lss |= dat[++bt] << 4 | (dat[++bt] & 3) << 12, lcs = dat[bt] >> 2 | dat[++bt] << 6;
      else lss |= dat[++bt] << 4 | (dat[++bt] & 63) << 12, lcs = dat[bt] >> 6 | dat[++bt] << 2 | dat[++bt] << 10;
    }
    ++bt;
    let buf = out ? out.subarray(st.y, st.y + st.m) : new u8(st.m);
    let spl = buf.length - lss;
    if (lbt == 0) buf.set(dat.subarray(bt, bt += lss), spl);
    else if (lbt == 1) fill(buf, dat[bt++], spl);
    else {
      let hu = st.h;
      if (lbt == 2) {
        const hud = rhu(dat, bt);
        lcs += bt - (bt = hud[0]);
        st.h = hu = hud[1];
      } else if (!hu) err(0);
      (s4 ? dhu4 : dhu)(dat.subarray(bt, bt += lcs), buf.subarray(spl), hu);
    }
    let ns = dat[bt++];
    if (ns) {
      if (ns == 255) ns = (dat[bt++] | dat[bt++] << 8) + 32512;
      else if (ns > 127) ns = ns - 128 << 8 | dat[bt++];
      const scm = dat[bt++];
      if (scm & 3) err(0);
      const dts = [
        dmlt,
        doct,
        dllt
      ];
      for (let i = 2; i > -1; --i) {
        const md = scm >> (i << 1) + 2 & 3;
        if (md == 1) {
          const rbuf = new u8([
            0,
            0,
            dat[bt++]
          ]);
          dts[i] = {
            s: rbuf.subarray(2, 3),
            n: rbuf.subarray(0, 1),
            t: new u16(rbuf.buffer, 0, 1),
            b: 0
          };
        } else if (md == 2) {
          [bt, dts[i]] = rfse(dat, bt, 9 - (i & 1));
        } else if (md == 3) {
          if (!st.t) err(0);
          dts[i] = st.t[i];
        }
      }
      const [mlt, oct, llt] = st.t = dts;
      const lb = dat[ebt - 1];
      if (!lb) err(0);
      let spos = (ebt << 3) - 8 + msb(lb) - llt.b, cbt = spos >> 3, oubt = 0;
      let lst = (dat[cbt] | dat[cbt + 1] << 8) >> (spos & 7) & (1 << llt.b) - 1;
      cbt = (spos -= oct.b) >> 3;
      let ost = (dat[cbt] | dat[cbt + 1] << 8) >> (spos & 7) & (1 << oct.b) - 1;
      cbt = (spos -= mlt.b) >> 3;
      let mst = (dat[cbt] | dat[cbt + 1] << 8) >> (spos & 7) & (1 << mlt.b) - 1;
      for (++ns; --ns; ) {
        const llc = llt.s[lst];
        const lbtr = llt.n[lst];
        const mlc = mlt.s[mst];
        const mbtr = mlt.n[mst];
        const ofc = oct.s[ost];
        const obtr = oct.n[ost];
        cbt = (spos -= ofc) >> 3;
        const ofp = 1 << ofc;
        let off = ofp + ((dat[cbt] | dat[cbt + 1] << 8 | dat[cbt + 2] << 16 | dat[cbt + 3] << 24) >>> (spos & 7) & ofp - 1);
        cbt = (spos -= mlb[mlc]) >> 3;
        let ml = mlbl[mlc] + ((dat[cbt] | dat[cbt + 1] << 8 | dat[cbt + 2] << 16) >> (spos & 7) & (1 << mlb[mlc]) - 1);
        cbt = (spos -= llb[llc]) >> 3;
        const ll = llbl[llc] + ((dat[cbt] | dat[cbt + 1] << 8 | dat[cbt + 2] << 16) >> (spos & 7) & (1 << llb[llc]) - 1);
        cbt = (spos -= lbtr) >> 3;
        lst = llt.t[lst] + ((dat[cbt] | dat[cbt + 1] << 8) >> (spos & 7) & (1 << lbtr) - 1);
        cbt = (spos -= mbtr) >> 3;
        mst = mlt.t[mst] + ((dat[cbt] | dat[cbt + 1] << 8) >> (spos & 7) & (1 << mbtr) - 1);
        cbt = (spos -= obtr) >> 3;
        ost = oct.t[ost] + ((dat[cbt] | dat[cbt + 1] << 8) >> (spos & 7) & (1 << obtr) - 1);
        if (off > 3) {
          st.o[2] = st.o[1];
          st.o[1] = st.o[0];
          st.o[0] = off -= 3;
        } else {
          const idx = off - (ll != 0);
          if (idx) {
            off = idx == 3 ? st.o[0] - 1 : st.o[idx];
            if (idx > 1) st.o[2] = st.o[1];
            st.o[1] = st.o[0];
            st.o[0] = off;
          } else off = st.o[0];
        }
        for (let i = 0; i < ll; ++i) {
          buf[oubt + i] = buf[spl + i];
        }
        oubt += ll, spl += ll;
        let stin = oubt - off;
        if (stin < 0) {
          let len = -stin;
          const bs = st.e + stin;
          if (len > ml) len = ml;
          for (let i = 0; i < len; ++i) {
            buf[oubt + i] = st.w[bs + i];
          }
          oubt += len, ml -= len, stin = 0;
        }
        for (let i = 0; i < ml; ++i) {
          buf[oubt + i] = buf[stin + i];
        }
        oubt += ml;
      }
      if (oubt != spl) {
        while (spl < buf.length) {
          buf[oubt++] = buf[spl++];
        }
      } else oubt = buf.length;
      if (out) st.y += oubt;
      else buf = slc(buf, 0, oubt);
    } else if (out) {
      st.y += lss;
      if (spl) {
        for (let i = 0; i < lss; ++i) {
          buf[i] = buf[spl + i];
        }
      }
    } else if (spl) buf = slc(buf, spl);
    st.b = ebt;
    return buf;
  }
  err(2);
};
var cct = (bufs, ol) => {
  if (bufs.length == 1) return bufs[0];
  const buf = new u8(ol);
  for (let i = 0, b = 0; i < bufs.length; ++i) {
    const chk = bufs[i];
    buf.set(chk, b);
    b += chk.length;
  }
  return buf;
};
function decompress(dat, buf) {
  const bufs = [], nb = +!buf;
  let bt = 0, ol = 0;
  for (; dat.length; ) {
    const st = rzfh(dat, nb || buf);
    if (typeof st == "object") {
      if (nb) {
        buf = null;
        if (st.w.length == st.u) {
          bufs.push(buf = st.w);
          ol += st.u;
        }
      } else {
        bufs.push(buf);
        st.e = 0;
      }
      for (; !st.l; ) {
        const blk = rzb(dat, st, buf);
        if (!blk) err(5);
        if (buf) st.e = st.y;
        else {
          bufs.push(blk);
          ol += blk.length;
          cpw(st.w, 0, blk.length);
          st.w.set(blk, st.w.length - blk.length);
        }
      }
      bt = st.b + st.c * 4;
    } else bt = st;
    dat = dat.subarray(bt);
  }
  return cct(bufs, ol);
}

// zenoh-web/client/zenoh_web.ts
var Priority = Object.freeze({
  REAL_TIME: 1,
  INTERACTIVE_HIGH: 2,
  INTERACTIVE_LOW: 3,
  DATA_HIGH: 4,
  DATA: 5,
  DATA_LOW: 6,
  BACKGROUND: 7
});
var CODECS = Object.freeze([
  "ros2-image",
  "ros2-compressed-image",
  "ros2-depth",
  "ros2-compressed-depth",
  "ros2-pointcloud2",
  "dimos-image",
  "dimos-compressed-image",
  "dimos-depth",
  "dimos-compressed-depth",
  "dimos-pointcloud2"
]);
function codecOutput(codec) {
  if (codec.endsWith("pointcloud2")) {
    return "pointcloud";
  }
  return codec.endsWith("depth") ? "depth" : "video";
}
var builtinCodecInfos = CODECS.map((name) => ({
  name,
  output: codecOutput(name) === "video" ? "video" : "data"
}));
var codecDecoders = /* @__PURE__ */ new Map();
function registerCodec(name, decoder) {
  if (typeof name !== "string" || name.length === 0) {
    throw new TypeError(`zenoh-web: registerCodec needs a codec name, got ${String(name)}`);
  }
  if (typeof decoder !== "function") {
    throw new TypeError(`zenoh-web: registerCodec("${name}") needs a decoder function`);
  }
  const existing = codecDecoders.get(name);
  if (existing !== void 0 && existing !== decoder) {
    throw new Error(`zenoh-web: a decoder for codec "${name}" is already registered`);
  }
  codecDecoders.set(name, decoder);
}
for (const name of CODECS) {
  if (codecOutput(name) === "depth") {
    registerCodec(name, (bytes, message) => message.depth = decodeDepth(bytes));
  } else if (codecOutput(name) === "pointcloud") {
    registerCodec(name, (bytes, message) => message.points = decodePointCloud(bytes));
  }
}
var backedUpBytes = 64 * 1024;
var resumeBytes = 16 * 1024;
var gatherTimeoutMs = 3e3;
var openTimeoutMs = 1e4;
var pingTimeoutMs = 3e3;
var reconnectDelayMs = 1e3;
var ackEveryBytes = 16 * 1024;
var ackDelayMs = 5;
var clockWindow = 16;
var initialClockPings = 5;
var putHeaderBytes = 8;
var maxPartialMessages = 8;
var subscribeOptionNames = /* @__PURE__ */ new Set([
  "delivery",
  "priority",
  "bandwidthPriority",
  "queueSize",
  "maxAge",
  "maxHz",
  "dangerousMinHz",
  "minQuality",
  "maxQuality",
  "qualityToHzTradeoff",
  "codec"
]);
var publisherOptionNames = /* @__PURE__ */ new Set([
  "delivery",
  "priority",
  "repeatMs",
  "latencyLimit"
]);
function checkNumber(name, value, isValid, expected) {
  if (value === void 0) {
    return;
  }
  if (typeof value !== "number" || Number.isNaN(value) || !isValid(value)) {
    throw new RangeError(`zenoh-web: ${name} must be ${expected}, got ${String(value)}`);
  }
}
function checkCommon(options, allowed, where) {
  for (const name of Object.keys(options)) {
    if (!allowed.has(name)) {
      throw new TypeError(`zenoh-web: unknown ${where} option "${name}" (allowed: ${[
        ...allowed
      ].join(", ")})`);
    }
  }
  const { delivery, priority } = options;
  if (delivery !== void 0 && delivery !== "latest" && delivery !== "reliable") {
    throw new TypeError(`zenoh-web: delivery must be "latest" or "reliable", got ${String(delivery)}`);
  }
  checkNumber("priority", priority, (v) => Number.isInteger(v) && v >= 1 && v <= 7, "an integer 1..7 (see Priority)");
}
function validateSubscribeOptions(options, codecs = builtinCodecInfos) {
  checkCommon(options, subscribeOptionNames, "subscribe");
  const isUnit = (v) => v >= 0 && v <= 1;
  checkNumber("bandwidthPriority", options.bandwidthPriority, (v) => Number.isFinite(v) && v >= 0, ">= 0");
  checkNumber("queueSize", options.queueSize, (v) => v === Infinity || Number.isInteger(v) && v >= 1, "an integer >= 1 or Infinity");
  checkNumber("maxAge", options.maxAge, (v) => Number.isFinite(v) && v > 0, "> 0 (ms)");
  checkNumber("maxHz", options.maxHz, (v) => Number.isFinite(v) && v > 0, "> 0");
  checkNumber("dangerousMinHz", options.dangerousMinHz, (v) => Number.isFinite(v) && v >= 0 && v <= (options.maxHz ?? Infinity), ">= 0 and <= maxHz");
  checkNumber("minQuality", options.minQuality, isUnit, "within 0..1");
  checkNumber("maxQuality", options.maxQuality, isUnit, "within 0..1");
  checkNumber("qualityToHzTradeoff", options.qualityToHzTradeoff, isUnit, "within 0..1");
  if ((options.minQuality ?? 0) > (options.maxQuality ?? 1)) {
    throw new RangeError("zenoh-web: minQuality must be <= maxQuality");
  }
  if (options.codec !== void 0) {
    const codec = codecs.find((info) => info.name === options.codec);
    if (codec === void 0) {
      throw new TypeError(`zenoh-web: unknown codec "${String(options.codec)}" (the bridge has: ${codecs.map((info) => info.name).join(", ")})`);
    }
    if (codec.output === "video" && options.delivery === "reliable") {
      throw new TypeError(`zenoh-web: ${options.codec} is a video codec (a lossy video track); use delivery "latest"`);
    }
  }
}
function validatePublisherOptions(options) {
  checkCommon(options, publisherOptionNames, "publisher");
  checkNumber("repeatMs", options.repeatMs, (v) => Number.isFinite(v) && v > 0, "> 0 (ms)");
  checkNumber("latencyLimit", options.latencyLimit, (v) => Number.isFinite(v) && v > 0, "> 0 (ms)");
}
function channelInit(delivery, maxAge) {
  if (delivery === "reliable") {
    return {
      ordered: true
    };
  }
  if (maxAge) {
    return {
      ordered: false,
      maxPacketLifeTime: Math.min(65535, Math.round(maxAge))
    };
  }
  return {
    ordered: false,
    maxRetransmits: 0
  };
}
function toBytes(value) {
  if (typeof value === "string") {
    return new TextEncoder().encode(value);
  }
  if (value instanceof Uint8Array) {
    return value;
  }
  if (ArrayBuffer.isView(value)) {
    return new Uint8Array(value.buffer, value.byteOffset, value.byteLength);
  }
  return new Uint8Array(value);
}
function fromBase64(text) {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index++) {
    bytes[index] = binary.charCodeAt(index);
  }
  return bytes;
}
function toBase64(bytes) {
  let binary = "";
  for (let index = 0; index < bytes.length; index++) {
    binary += String.fromCharCode(bytes[index]);
  }
  return btoa(binary);
}
var keyDecoder = new TextDecoder();
function decodeFrame(buffer) {
  const view = new DataView(buffer);
  const keyLength = view.getUint16(0, true);
  const key = keyDecoder.decode(new Uint8Array(buffer, 2, keyLength));
  const offset = 2 + keyLength;
  return {
    key,
    timestamp: view.getFloat64(offset, true),
    seq: view.getUint32(offset + 8, true),
    frameId: view.getUint32(offset + 12, true),
    chunkIndex: view.getUint32(offset + 16, true),
    chunkCount: view.getUint32(offset + 20, true),
    chunk: new Uint8Array(buffer, offset + 24)
  };
}
var depthEncodings = {
  1: "16UC1",
  2: "32FC1",
  3: "mono16"
};
function decodeDepth(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes[0] !== 1) {
    throw new Error(`zenoh-web: unknown depth format version ${bytes[0]}`);
  }
  const encoding = depthEncodings[bytes[1]];
  if (!encoding) {
    throw new Error(`zenoh-web: unknown depth encoding ${bytes[1]}`);
  }
  const width = view.getUint32(4, true);
  const height = view.getUint32(8, true);
  const values = decompress(bytes.subarray(20));
  const aligned = values.byteOffset % 4 === 0 ? values : values.slice();
  const count = width * height;
  const data = encoding === "32FC1" ? new Float32Array(aligned.buffer, aligned.byteOffset, count) : new Uint16Array(aligned.buffer, aligned.byteOffset, count);
  return {
    width,
    height,
    sourceWidth: view.getUint32(12, true),
    sourceHeight: view.getUint32(16, true),
    stride: view.getUint16(2, true),
    encoding,
    data
  };
}
function decodePointCloud(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes[0] !== 1) {
    throw new Error(`zenoh-web: unknown point cloud format version ${bytes[0]}`);
  }
  const hasIntensity = (bytes[1] & 1) === 1;
  const count = view.getUint32(4, true);
  const origin = [
    view.getFloat32(12, true),
    view.getFloat32(16, true),
    view.getFloat32(20, true)
  ];
  const scale = view.getFloat32(24, true);
  const voxelSize = view.getFloat32(28, true);
  const body = decompress(bytes.subarray(40));
  const quantized = new DataView(body.buffer, body.byteOffset, body.byteLength);
  const positions = new Float32Array(count * 3);
  for (let index = 0; index < count * 3; index++) {
    positions[index] = origin[index % 3] + quantized.getInt16(index * 2, true) * scale;
  }
  return {
    count,
    sourceCount: view.getUint32(8, true),
    positions,
    intensity: hasIntensity ? body.slice(count * 6, count * 7) : null,
    intensityMin: view.getFloat32(32, true),
    intensityScale: view.getFloat32(36, true),
    origin,
    scale,
    voxelSize,
    maxError: voxelSize > 0 ? voxelSize / 2 : scale / 2
  };
}
function decodeVideoFrameInfo(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return {
    keyframe: (bytes[1] & 1) === 1,
    width: view.getUint32(4, true),
    height: view.getUint32(8, true),
    sourceWidth: view.getUint32(12, true),
    sourceHeight: view.getUint32(16, true),
    quality: view.getFloat32(20, true),
    encodedBytes: view.getUint32(24, true)
  };
}
function encodePut(payload, sentAtMs) {
  const frame = new Uint8Array(putHeaderBytes + payload.length);
  new DataView(frame.buffer).setFloat64(0, sentAtMs, true);
  frame.set(payload, putHeaderBytes);
  return frame;
}
var sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
function waitOpen(channel, timeoutMs) {
  if (channel.readyState === "open") {
    return Promise.resolve();
  }
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("data channel open timed out")), timeoutMs);
    channel.addEventListener("open", () => {
      clearTimeout(timer);
      resolve();
    }, {
      once: true
    });
    channel.addEventListener("close", () => {
      clearTimeout(timer);
      reject(new Error("data channel closed before opening"));
    }, {
      once: true
    });
  });
}
function waitIceGathering(peer) {
  if (peer.iceGatheringState === "complete") {
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    const finish = () => {
      clearTimeout(timer);
      peer.removeEventListener("icegatheringstatechange", onChange);
      resolve();
    };
    const onChange = () => {
      if (peer.iceGatheringState === "complete") {
        finish();
      }
    };
    const timer = setTimeout(finish, gatherTimeoutMs);
    peer.addEventListener("icegatheringstatechange", onChange);
  });
}
var Acceptance = class {
  promise;
  settled = false;
  #bridgeAccepted = false;
  #channelOpen = false;
  #resolve = () => {
  };
  #reject = () => {
  };
  constructor() {
    this.promise = new Promise((resolve, reject) => {
      this.#resolve = resolve;
      this.#reject = reject;
    });
    this.promise.catch(() => {
    });
  }
  bridgeAccepted() {
    this.#bridgeAccepted = true;
    this.#settleIfReady();
  }
  channelOpened() {
    this.#channelOpen = true;
    this.#settleIfReady();
  }
  #settleIfReady() {
    if (!this.settled && this.#bridgeAccepted && this.#channelOpen) {
      this.settled = true;
      this.#resolve();
    }
  }
  reject(error) {
    if (!this.settled) {
      this.settled = true;
      this.#reject(error);
    }
  }
};
var Endpoint = class {
  owner;
  id;
  key;
  channel;
  closed;
  rejectionReason;
  bridgeStats;
  acceptance;
  constructor(owner, id, key) {
    this.owner = owner;
    this.id = id;
    this.key = key;
    this.channel = null;
    this.closed = false;
    this.rejectionReason = null;
    this.bridgeStats = null;
    this.acceptance = new Acceptance();
  }
  /** Resolves once the bridge accepted this channel; rejects with the bridge's reason otherwise. */
  ready() {
    return this.acceptance.promise;
  }
  /** Starts a new attempt to get accepted (each attach, including reconnects). */
  beginAttempt() {
    this.acceptance = new Acceptance();
    return this.acceptance;
  }
  watchChannel(channel, acceptance) {
    waitOpen(channel, openTimeoutMs).then(() => acceptance.channelOpened(), (error) => acceptance.reject(error));
  }
  _accepted() {
    this.acceptance.bridgeAccepted();
  }
  _rejected(reason) {
    this.rejectionReason = reason;
    this.acceptance.reject(new Error(`zenoh-web: bridge rejected ${this.key}: ${reason}`));
    this.owner._forget(this);
  }
  close() {
    if (this.closed) {
      return;
    }
    this.closed = true;
    this.channel?.close();
    this.owner._forget(this);
  }
};
var Subscription = class extends Endpoint {
  options;
  callback;
  received;
  /** chunked messages dropped incomplete (lost chunk or abandoned for a newer message) */
  partialDropped;
  /** codec payloads that failed to decode in this page */
  decodeErrors;
  /** video codecs: the decoded video (also on each message as `mediaStream`) */
  mediaStream;
  /** where the codec's output arrives (null: no codec, raw bytes) */
  codecKind;
  #warnedNoDecoder;
  #videoTransceiver;
  /** drops before the current channel (each new channel restarts seq at 0) */
  #droppedBefore;
  #firstSeq;
  #maxSeq;
  #receivedOnChannel;
  #highestConsumedFrame;
  #bytesSinceAck;
  #ackTimer;
  #partials;
  constructor(owner, id, key, options, callback) {
    super(owner, id, key), this.options = options, this.callback = callback, this.received = 0, this.partialDropped = 0, this.decodeErrors = 0, this.mediaStream = null, this.#warnedNoDecoder = false, this.#videoTransceiver = null, this.#droppedBefore = 0, this.#firstSeq = -1, this.#maxSeq = -1, this.#receivedOnChannel = 0, this.#highestConsumedFrame = -1, this.#bytesSinceAck = 0, this.#ackTimer = null, this.#partials = /* @__PURE__ */ new Map();
    this.codecKind = options.codec === void 0 ? null : owner.codecs.find((info) => info.name === options.codec)?.output ?? "data";
  }
  get state() {
    if (this.closed) {
      return "closed";
    }
    if (this.rejectionReason !== null) {
      return "rejected";
    }
    return this.acceptance.settled ? "open" : "connecting";
  }
  /** messages the bridge accepted for us but we never got whole (queue, age, maxHz, network) */
  get dropped() {
    const span = this.#maxSeq < 0 ? 0 : this.#maxSeq - this.#firstSeq + 1;
    return this.#droppedBefore + Math.max(0, span - this.#receivedOnChannel);
  }
  attach(peer) {
    this.#droppedBefore = this.dropped;
    this.#firstSeq = -1;
    this.#maxSeq = -1;
    this.#receivedOnChannel = 0;
    this.#highestConsumedFrame = -1;
    this.#bytesSinceAck = 0;
    this.#partials.clear();
    const acceptance = this.beginAttempt();
    if (this.codecKind !== "video") {
      this.#openChannel(peer, acceptance, null);
      return;
    }
    this.#videoTransceiver = null;
    this.owner._acquireVideoTransceiver(peer).then((transceiver) => {
      if (this.closed || acceptance !== this.acceptance) {
        this.owner._releaseVideoTransceiver(peer, transceiver);
        return;
      }
      this.#videoTransceiver = transceiver;
      this.mediaStream = new MediaStream([
        transceiver.receiver.track
      ]);
      this.#openChannel(peer, acceptance, transceiver.mid);
    }, (error) => acceptance.reject(new Error(`zenoh-web: video renegotiation for ${this.key} failed: ${error.message}`)));
  }
  #openChannel(peer, acceptance, mid) {
    const label = JSON.stringify({
      type: "sub",
      key: this.key,
      id: this.id,
      opts: this.options,
      ...mid === null ? {} : {
        mid
      }
    });
    const channel = peer.createDataChannel(label, channelInit(this.options.delivery, this.options.maxAge));
    channel.binaryType = "arraybuffer";
    channel.onmessage = (event) => {
      if (event.data instanceof ArrayBuffer) {
        this.#onFrame(channel, decodeFrame(event.data), event.data.byteLength);
      }
    };
    this.channel = channel;
    this.watchChannel(channel, acceptance);
  }
  close() {
    if (this.closed) {
      return;
    }
    super.close();
    const peer = this.owner._peer;
    if (this.#videoTransceiver && peer) {
      this.owner._releaseVideoTransceiver(peer, this.#videoTransceiver);
    }
    this.#videoTransceiver = null;
  }
  #onFrame(channel, frame, frameBytes) {
    if (frame.chunkCount <= 1) {
      this.#deliver({
        key: frame.key,
        bytes: frame.chunk,
        timestamp: frame.timestamp,
        seq: frame.seq
      });
    } else {
      this.#addChunk(frame);
    }
    this.#consumed(channel, frame.frameId, frameBytes);
  }
  #addChunk(frame) {
    let partial = this.#partials.get(frame.seq);
    if (!partial) {
      partial = {
        key: frame.key,
        timestamp: frame.timestamp,
        chunks: new Array(frame.chunkCount),
        receivedChunks: 0,
        receivedBytes: 0
      };
      this.#partials.set(frame.seq, partial);
      this.#evictPartials(maxPartialMessages);
    }
    if (partial.chunks[frame.chunkIndex] === void 0) {
      partial.chunks[frame.chunkIndex] = frame.chunk.slice();
      partial.receivedChunks++;
      partial.receivedBytes += frame.chunk.length;
    }
    if (partial.receivedChunks < partial.chunks.length) {
      return;
    }
    this.#partials.delete(frame.seq);
    const bytes = new Uint8Array(partial.receivedBytes);
    let offset = 0;
    for (const chunk of partial.chunks) {
      const piece = chunk;
      bytes.set(piece, offset);
      offset += piece.length;
    }
    for (const seq of [
      ...this.#partials.keys()
    ]) {
      if (seq < frame.seq) {
        this.#partials.delete(seq);
        this.partialDropped++;
      }
    }
    this.#deliver({
      key: partial.key,
      bytes,
      timestamp: partial.timestamp,
      seq: frame.seq
    });
  }
  #evictPartials(limit) {
    while (this.#partials.size > limit) {
      const oldest = Math.min(...this.#partials.keys());
      this.#partials.delete(oldest);
      this.partialDropped++;
    }
  }
  /** Adds the codec's decoded form; false if it can't be decoded. */
  #decode(message) {
    try {
      if (this.codecKind === "video") {
        message.video = decodeVideoFrameInfo(message.bytes);
        message.mediaStream = this.mediaStream ?? void 0;
        return true;
      }
      const name = String(this.options.codec);
      const decoder = codecDecoders.get(name);
      if (decoder !== void 0) {
        message.decoded = decoder(message.bytes, message);
      } else if (!this.#warnedNoDecoder) {
        this.#warnedNoDecoder = true;
        console.warn(`zenoh-web: no decoder registered for codec "${name}" (registerCodec("${name}", decoder)); msg.bytes carries its encoded bytes`);
      }
      return true;
    } catch (error) {
      this.decodeErrors++;
      console.error(`zenoh-web: ${this.options.codec} payload on ${message.key} did not decode`, error);
      return false;
    }
  }
  #deliver(message) {
    if (this.codecKind !== null && !this.#decode(message)) {
      return;
    }
    this.received++;
    this.#receivedOnChannel++;
    if (this.#firstSeq < 0 || message.seq < this.#firstSeq) {
      this.#firstSeq = message.seq;
    }
    if (message.seq > this.#maxSeq) {
      this.#maxSeq = message.seq;
    }
    try {
      this.callback(message);
    } catch (error) {
      console.error(`zenoh-web: subscriber callback for ${this.key} threw`, error);
    }
  }
  /** Tells the bridge we processed every frame up to frameId: 4 bytes, little endian. */
  #consumed(channel, frameId, byteLength) {
    if (frameId > this.#highestConsumedFrame) {
      this.#highestConsumedFrame = frameId;
    }
    this.#bytesSinceAck += byteLength;
    const sendAck = () => {
      this.#ackTimer = null;
      if (channel.readyState !== "open" || channel !== this.channel) {
        return;
      }
      this.#bytesSinceAck = 0;
      const ack = new Uint8Array(4);
      new DataView(ack.buffer).setUint32(0, this.#highestConsumedFrame, true);
      channel.send(ack);
    };
    if (this.#bytesSinceAck >= ackEveryBytes) {
      if (this.#ackTimer) {
        clearTimeout(this.#ackTimer);
      }
      sendAck();
    } else if (!this.#ackTimer) {
      this.#ackTimer = setTimeout(sendAck, ackDelayMs);
    }
  }
};
var Publisher = class extends Endpoint {
  options;
  sent;
  dropped;
  tripped;
  /** why the deadman fired: "heartbeat" | "disconnected" | "shutdown" */
  tripReason;
  deadmanArmed;
  #last;
  /** stamped frames waiting for the channel (reliable: all, latest: only the newest) */
  #pending;
  #repeatTimer;
  #tripListeners;
  constructor(owner, id, key, options) {
    super(owner, id, key), this.options = options, this.sent = 0, this.dropped = 0, this.tripped = false, this.tripReason = null, this.deadmanArmed = false, this.#last = null, this.#pending = [], this.#repeatTimer = null, this.#tripListeners = /* @__PURE__ */ new Set();
    if (options.repeatMs) {
      this.#repeatTimer = setInterval(() => {
        if (this.#last && !this.tripped && this.rejectionReason === null) {
          this.#send(encodePut(this.#last, this.owner.now()));
        }
      }, options.repeatMs);
    }
  }
  get state() {
    if (this.closed) {
      return "closed";
    }
    if (this.rejectionReason !== null) {
      return "rejected";
    }
    if (this.tripped) {
      return "tripped";
    }
    return this.acceptance.settled ? "open" : "connecting";
  }
  onTripped(listener) {
    this.#tripListeners.add(listener);
    return () => {
      this.#tripListeners.delete(listener);
    };
  }
  attach(peer) {
    const { delivery, priority, latencyLimit } = this.options;
    const label = JSON.stringify({
      type: "pub",
      key: this.key,
      id: this.id,
      opts: {
        delivery,
        priority,
        latencyLimit
      }
    });
    const acceptance = this.beginAttempt();
    const channel = peer.createDataChannel(label, channelInit(delivery, void 0));
    channel.binaryType = "arraybuffer";
    channel.bufferedAmountLowThreshold = resumeBytes;
    channel.onbufferedamountlow = () => this.#flush();
    this.channel = channel;
    this.watchChannel(channel, acceptance);
    this.acceptance.promise.then(() => this.#flush(), () => {
    });
  }
  #checkUsable() {
    if (this.closed) {
      throw new Error(`zenoh-web: publisher ${this.key} is closed`);
    }
    if (this.rejectionReason !== null) {
      throw new Error(`zenoh-web: publisher ${this.key} was rejected by the bridge: ${this.rejectionReason}`);
    }
    if (this.tripped) {
      throw new Error(`zenoh-web: publisher ${this.key} is tripped (deadman fired: ${this.tripReason}); create a new publisher`);
    }
  }
  /** `timestamp`: when the value was produced, in this client's clock (`z.now()`); defaults to now. */
  put(value, { timestamp } = {}) {
    this.#checkUsable();
    const bytes = toBytes(value);
    this.#last = bytes;
    this.#send(encodePut(bytes, timestamp ?? this.owner.now()));
  }
  #send(frame) {
    const channel = this.channel;
    const backedUp = !channel || channel.readyState !== "open" || !this.acceptance.settled || channel.bufferedAmount > backedUpBytes;
    if (backedUp || this.#pending.length > 0) {
      if (this.options.delivery !== "reliable") {
        this.dropped += this.#pending.length;
        this.#pending = [];
      }
      this.#pending.push(frame);
      return;
    }
    channel.send(frame);
    this.sent++;
  }
  #flush() {
    const channel = this.channel;
    while (this.#pending.length > 0 && channel?.readyState === "open" && this.rejectionReason === null && channel.bufferedAmount <= backedUpBytes) {
      channel.send(this.#pending.shift());
      this.sent++;
    }
  }
  /**
     * Stores `value` on the bridge; it is published once (REAL_TIME, reliable) if this frontend's
     * heartbeat stops, it disconnects, or the bridge shuts down. Then this publisher is tripped.
     */
  setDeadman(value) {
    if (!this.owner.options.heartbeatHz) {
      throw new Error("zenoh-web: setDeadman needs a heartbeat; connect(url, { heartbeatHz: 5, heartbeatMisses: 3 })");
    }
    this.#checkUsable();
    const bytes = toBytes(value);
    return this.ready().then(async () => {
      await this.owner._request({
        op: "setDeadman",
        pubId: this.id,
        bytes: toBase64(bytes)
      }, pingTimeoutMs);
      this.deadmanArmed = true;
    });
  }
  async clearDeadman() {
    this.#checkUsable();
    await this.owner._request({
      op: "clearDeadman",
      pubId: this.id
    }, pingTimeoutMs);
    this.deadmanArmed = false;
  }
  _trip(reason) {
    if (this.tripped || this.closed) {
      return;
    }
    this.tripped = true;
    this.tripReason = reason;
    this.deadmanArmed = false;
    this.#pending = [];
    if (this.#repeatTimer) {
      clearInterval(this.#repeatTimer);
    }
    this.owner._forget(this);
    for (const listener of this.#tripListeners) {
      try {
        listener(reason);
      } catch (error) {
        console.error("zenoh-web: onTripped listener threw", error);
      }
    }
  }
  _rejected(reason) {
    this.#pending = [];
    if (this.#repeatTimer) {
      clearInterval(this.#repeatTimer);
    }
    super._rejected(reason);
  }
  close() {
    if (this.#repeatTimer) {
      clearInterval(this.#repeatTimer);
    }
    super.close();
  }
};
var ZenohWeb = class {
  state = "connecting";
  /** per subscribed/published key expression */
  stats = {};
  /** latest round trip to the bridge (heartbeat, else control ping) */
  rttMs = null;
  /** bridge clock minus this client's clock, from the lowest-RTT recent sample */
  clockOffsetMs = null;
  /** bridge-side heartbeat, clock and access-control stats */
  bridgeStats = null;
  /** the codecs the bridge runs (built-in and its host application's), fetched on connect */
  codecs = builtinCodecInfos;
  url;
  options;
  /** this client's clock in ms; put timestamps and clock sync use it */
  now;
  #peer = null;
  #control = null;
  #heartbeat = null;
  #heartbeatTimer = null;
  #heartbeatPaused = false;
  #clockSamples = [];
  #endpoints = /* @__PURE__ */ new Set();
  /** every endpoint by id, including tripped/rejected ones the bridge may still talk about */
  #endpointsById = /* @__PURE__ */ new Map();
  #requests = /* @__PURE__ */ new Map();
  #stateListeners = /* @__PURE__ */ new Set();
  #nextId = 1;
  #closed = false;
  #generation = 0;
  #statsTimer = null;
  /** renegotiations run one at a time */
  #negotiation = Promise.resolve();
  /** resolves once the current peer connection is up (renegotiation needs `control`) */
  #connected = new Promise(() => {
  });
  #markConnected = () => {
  };
  /** video transceivers of closed subscriptions, reused before adding new ones */
  #freeVideoTransceivers = /* @__PURE__ */ new Map();
  constructor(url, options = {}) {
    this.url = url.replace(/\/+$/, "");
    this.options = {
      iceServers: [],
      reconnect: true,
      statsIntervalMs: 1e3,
      heartbeatHz: 0,
      heartbeatMisses: 3,
      ...options
    };
    checkNumber("heartbeatHz", this.options.heartbeatHz, (v) => Number.isFinite(v) && v >= 0, ">= 0 (0 = no heartbeat)");
    checkNumber("heartbeatMisses", this.options.heartbeatMisses, (v) => Number.isInteger(v) && v >= 1, "an integer >= 1");
    checkNumber("bandwidthTargetFraction", this.options.bandwidthTargetFraction, (v) => v > 0 && v <= 1, "within (0, 1]");
    this.now = this.options.clock ?? (() => performance.timeOrigin + performance.now());
  }
  onState(listener) {
    this.#stateListeners.add(listener);
    return () => {
      this.#stateListeners.delete(listener);
    };
  }
  #setState(state) {
    if (state === this.state) {
      return;
    }
    this.state = state;
    for (const listener of this.#stateListeners) {
      try {
        listener(state);
      } catch (error) {
        console.error("zenoh-web: state listener threw", error);
      }
    }
  }
  /** NTP-style sample: t0/t3 in our clock, t1/t2 bridge receive/send in its clock. */
  #addClockSample(t0, t1, t2, t3) {
    const rttMs = t3 - t0 - (t2 - t1);
    const offsetMs = (t1 - t0 + (t2 - t3)) / 2;
    if (!Number.isFinite(rttMs) || !Number.isFinite(offsetMs)) {
      return;
    }
    this.#clockSamples.push({
      offsetMs,
      rttMs
    });
    if (this.#clockSamples.length > clockWindow) {
      this.#clockSamples.shift();
    }
    const best = this.#clockSamples.reduce((a, b) => b.rttMs < a.rttMs ? b : a);
    this.clockOffsetMs = best.offsetMs;
    this.rttMs = rttMs;
  }
  /** Clock-sync ping over control; also reports our current estimate to the bridge. */
  async #controlPing() {
    const t0 = this.now();
    const response = await this._request({
      op: "ping",
      t0,
      offsetMs: this.clockOffsetMs,
      rttMs: this.rttMs
    }, pingTimeoutMs);
    this.#addClockSample(t0, Number(response.t1), Number(response.t2), this.now());
  }
  /** Opens (or re-opens) the peer connection and every channel on it. */
  get _peer() {
    return this.#peer;
  }
  /**
     * A recvonly video transceiver bound to a bridge track: a free one, or a new one added through
     * a renegotiation over `control` (the bridge answers with a track for the new m-line).
     */
  _acquireVideoTransceiver(peer) {
    const free = this.#freeVideoTransceivers.get(peer)?.pop();
    if (free) {
      return Promise.resolve(free);
    }
    const run = async () => {
      await this.#connected;
      if (peer !== this.#peer) {
        throw new Error("connection replaced");
      }
      const transceiver = peer.addTransceiver("video", {
        direction: "recvonly"
      });
      await peer.setLocalDescription(await peer.createOffer());
      const offer = peer.localDescription;
      const response = await this._request({
        op: "renegotiate",
        addVideo: true,
        sdp: {
          type: offer?.type,
          sdp: offer?.sdp
        }
      }, openTimeoutMs);
      await peer.setRemoteDescription(response.sdp);
      if (response.mid !== transceiver.mid) {
        throw new Error(`bridge bound mid ${String(response.mid)}, expected ${String(transceiver.mid)}`);
      }
      return transceiver;
    };
    const result = this.#negotiation.then(run, run);
    this.#negotiation = result.catch(() => {
    });
    return result;
  }
  _releaseVideoTransceiver(peer, transceiver) {
    if (peer === this.#peer && peer.connectionState !== "closed") {
      const free = this.#freeVideoTransceivers.get(peer) ?? [];
      free.push(transceiver);
      this.#freeVideoTransceivers.set(peer, free);
    }
  }
  async _open() {
    const generation = ++this.#generation;
    this.#setState("connecting");
    this.#clockSamples = [];
    this.#connected = new Promise((resolve) => {
      this.#markConnected = resolve;
    });
    if (this.#peer) {
      this.#freeVideoTransceivers.delete(this.#peer);
    }
    const peer = new RTCPeerConnection({
      iceServers: this.options.iceServers
    });
    const control = peer.createDataChannel("control", {
      ordered: true
    });
    this.#peer = peer;
    this.#control = control;
    control.onmessage = (event) => this.#onControlMessage(String(event.data));
    control.onclose = () => this.#onLost(generation);
    peer.onconnectionstatechange = () => {
      if (generation !== this.#generation) {
        return;
      }
      const connectionState = peer.connectionState;
      if (connectionState === "failed" || connectionState === "closed") {
        this.#onLost(generation);
      } else if (connectionState === "disconnected") {
        this.#setState("degraded");
      } else if (connectionState === "connected" && control.readyState === "open") {
        this.#setState("connected");
      }
    };
    if (this.options.heartbeatHz > 0) {
      this.#attachHeartbeat(peer);
    }
    for (const endpoint of this.#endpoints) {
      endpoint.attach(peer);
    }
    try {
      await peer.setLocalDescription(await peer.createOffer());
      await waitIceGathering(peer);
      const response = await fetch(`${this.url}/offer`, {
        method: "POST",
        headers: {
          "content-type": "application/json"
        },
        body: JSON.stringify({
          type: peer.localDescription?.type,
          sdp: peer.localDescription?.sdp
        })
      });
      if (!response.ok) {
        throw new Error(`bridge refused offer: ${response.status} ${await response.text()}`);
      }
      await peer.setRemoteDescription(await response.json());
      await waitOpen(control, openTimeoutMs);
      if (this.options.bandwidthTargetFraction !== void 0) {
        await this._request({
          op: "configure",
          bandwidthTargetFraction: this.options.bandwidthTargetFraction
        }, pingTimeoutMs);
      }
      this.codecs = Object.freeze((await this._request({
        op: "codecs"
      }, pingTimeoutMs)).codecs);
      for (let index = 0; index < initialClockPings; index++) {
        await this.#controlPing();
      }
    } catch (error) {
      this.#onLost(generation);
      throw error;
    }
    this.#setState("connected");
    this.#markConnected();
  }
  #attachHeartbeat(peer) {
    const label = JSON.stringify({
      type: "heartbeat",
      opts: {
        hz: this.options.heartbeatHz,
        misses: this.options.heartbeatMisses
      }
    });
    const heartbeat = peer.createDataChannel(label, {
      ordered: false,
      maxRetransmits: 0
    });
    heartbeat.onmessage = (event) => {
      try {
        const reply = JSON.parse(String(event.data));
        this.#addClockSample(reply.t0, reply.t1, reply.t2, this.now());
      } catch {
      }
    };
    this.#heartbeat = heartbeat;
    if (!this.#heartbeatTimer) {
      this.#heartbeatTimer = setInterval(() => {
        const channel = this.#heartbeat;
        if (this.#heartbeatPaused || channel?.readyState !== "open") {
          return;
        }
        channel.send(JSON.stringify({
          t0: this.now(),
          offsetMs: this.clockOffsetMs,
          rttMs: this.rttMs
        }));
      }, 1e3 / this.options.heartbeatHz);
    }
  }
  /** Stops sending heartbeats (the bridge then fires this frontend's deadmen); for testing deadman wiring. */
  pauseHeartbeat() {
    this.#heartbeatPaused = true;
  }
  resumeHeartbeat() {
    this.#heartbeatPaused = false;
  }
  #tripArmedPublishers(reason) {
    for (const endpoint of this.#endpointsById.values()) {
      if (endpoint instanceof Publisher && endpoint.deadmanArmed) {
        endpoint._trip(reason);
      }
    }
  }
  #onLost(generation) {
    if (generation !== this.#generation || this.state === "lost") {
      return;
    }
    this.#setState("lost");
    this.#tripArmedPublishers("disconnected");
    for (const request of this.#requests.values()) {
      clearTimeout(request.timer);
      request.reject(new Error("connection lost"));
    }
    this.#requests.clear();
    this.#peer?.close();
    if (this.options.reconnect && !this.#closed) {
      setTimeout(async () => {
        while (!this.#closed && generation === this.#generation) {
          try {
            await this._open();
            return;
          } catch (error) {
            console.warn("zenoh-web: reconnect failed", error);
            await sleep(reconnectDelayMs);
          }
        }
      }, reconnectDelayMs);
    }
  }
  #onControlMessage(text) {
    let response;
    try {
      response = JSON.parse(text);
    } catch {
      return;
    }
    if (response.event !== void 0) {
      const endpoint = this.#endpointsById.get(Number(response.id));
      if (response.event === "tripped" && endpoint instanceof Publisher) {
        endpoint._trip(String(response.reason));
      } else if (response.event === "rejected") {
        endpoint?._rejected(String(response.reason));
      } else if (response.event === "accepted") {
        endpoint?._accepted();
      }
      return;
    }
    const request = this.#requests.get(Number(response.id));
    if (!request) {
      return;
    }
    this.#requests.delete(Number(response.id));
    clearTimeout(request.timer);
    if (response.ok) {
      request.resolve(response);
    } else {
      request.reject(new Error(`zenoh-web bridge: ${response.error ?? "error"}`));
    }
  }
  _request(body, timeoutMs) {
    const control = this.#control;
    if (!control || control.readyState !== "open") {
      return Promise.reject(new Error(`not connected (${this.state})`));
    }
    const id = this.#nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.#requests.delete(id);
        reject(new Error(`${body.op} timed out`));
      }, timeoutMs);
      this.#requests.set(id, {
        resolve,
        reject,
        timer
      });
      control.send(JSON.stringify({
        id,
        ...body
      }));
    });
  }
  subscribe(key, options, callback) {
    validateSubscribeOptions(options ?? {}, this.codecs);
    const subscription = new Subscription(this, this.#nextId++, key, {
      ...options
    }, callback);
    this.#addEndpoint(subscription);
    return subscription;
  }
  publisher(key, options = {}) {
    validatePublisherOptions(options);
    const publisher = new Publisher(this, this.#nextId++, key, {
      ...options
    });
    this.#addEndpoint(publisher);
    return publisher;
  }
  #addEndpoint(endpoint) {
    this.#endpoints.add(endpoint);
    this.#endpointsById.set(endpoint.id, endpoint);
    if (this.#peer && this.#peer.connectionState !== "closed") {
      endpoint.attach(this.#peer);
    }
  }
  /** Stops re-attaching an endpoint on reconnect (closed, tripped or rejected). */
  _forget(endpoint) {
    this.#endpoints.delete(endpoint);
    if (endpoint.closed) {
      this.#endpointsById.delete(endpoint.id);
    }
    this.#refreshStats(null);
  }
  /** zenoh query. */
  async get(key, { timeoutMs = 5e3 } = {}) {
    const response = await this._request({
      op: "get",
      key,
      timeoutMs
    }, timeoutMs + 2e3);
    const replies = response.replies;
    return replies.map((reply) => {
      if (reply.error !== void 0) {
        return {
          key: null,
          bytes: fromBase64(reply.error),
          error: true
        };
      }
      return {
        key: reply.key ?? null,
        bytes: fromBase64(reply.bytes ?? "")
      };
    });
  }
  /**
     * Keys currently live on the zenoh network under `filter`, including ones never subscribed to.
     * See SPEC.md "Topic enumeration" for which kinds of keys can and can't be seen.
     * `probeMs: 0` lists declarations and tokens only, without subscribing to `filter`.
     */
  async listTopics(filter = "**", { probeMs = 600 } = {}) {
    const response = await this._request({
      op: "listTopics",
      key: filter,
      probeMs
    }, probeMs + 5e3);
    return response.topics;
  }
  /** Polls bridge stats (and, without a heartbeat, clock sync) once; also runs on a timer while connected. */
  async pollStats() {
    try {
      await this.#controlPing();
      if (this.state === "degraded" && this.#peer?.connectionState === "connected") {
        this.#setState("connected");
      }
    } catch {
      if (this.state === "connected") {
        this.#setState("degraded");
      }
      return;
    }
    const response = await this._request({
      op: "stats"
    }, pingTimeoutMs).catch(() => null);
    if (response) {
      this.bridgeStats = {
        clock: response.clock,
        heartbeat: response.heartbeat,
        access: response.access,
        bandwidth: response.bandwidth ?? null
      };
    }
    this.#refreshStats(response?.channels ?? null);
  }
  #refreshStats(bridgeChannels) {
    if (bridgeChannels) {
      const byId = new Map(bridgeChannels.map((channel) => [
        channel.id,
        channel
      ]));
      for (const endpoint of this.#endpointsById.values()) {
        endpoint.bridgeStats = byId.get(endpoint.id) ?? null;
      }
    }
    const stats = {};
    for (const endpoint of this.#endpoints) {
      const entry = stats[endpoint.key] ??= {
        received: 0,
        dropped: 0,
        backlogBytes: 0,
        rttMs: this.rttMs,
        bridge: null
      };
      const bridge = endpoint.bridgeStats;
      entry.bridge = bridge;
      if (endpoint instanceof Subscription) {
        entry.received += endpoint.received;
        entry.dropped += endpoint.dropped;
        entry.backlogBytes += bridge ? Number(bridge.stats.queuedBytes) + Number(bridge.stats.outstandingBytes) : 0;
      } else {
        entry.received += endpoint.sent;
        entry.dropped += endpoint.dropped + (bridge ? Number(bridge.stats.droppedStale) : 0);
        entry.backlogBytes += endpoint.channel?.bufferedAmount ?? 0;
      }
    }
    this.stats = stats;
  }
  _startStats() {
    this.#statsTimer = setInterval(() => {
      if (this.state === "connected" || this.state === "degraded") {
        this.pollStats().catch(() => {
        });
      }
    }, this.options.statsIntervalMs);
  }
  close() {
    this.#closed = true;
    if (this.#statsTimer) {
      clearInterval(this.#statsTimer);
    }
    if (this.#heartbeatTimer) {
      clearInterval(this.#heartbeatTimer);
    }
    for (const endpoint of [
      ...this.#endpoints
    ]) {
      endpoint.close();
    }
    this.#generation++;
    this.#peer?.close();
    this.#setState("lost");
  }
};
async function connect(url, options = {}) {
  const client = new ZenohWeb(url, options);
  await client._open();
  client._startStats();
  return client;
}
export {
  CODECS,
  Priority,
  Publisher,
  Subscription,
  ZenohWeb,
  codecOutput,
  connect,
  decodeDepth,
  decodeFrame,
  decodePointCloud,
  decodeVideoFrameInfo,
  encodePut,
  registerCodec,
  validatePublisherOptions,
  validateSubscribeOptions
};
