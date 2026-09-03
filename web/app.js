/* t-fit front end. Plain JS, no build step, no dependencies. */
"use strict";

const $ = (s) => document.querySelector(s);
const api = async (path, opts) => {
  const r = await fetch(path, opts);
  if (!r.ok) {
    let msg = r.statusText;
    try { msg = (await r.json()).error || msg; } catch {}
    throw new Error(msg);
  }
  return r.status === 204 ? null : r.json();
};

const state = {
  entries: [],      // ascending by date
  stats: null,
  goal: null,        // the current goal, or null
  goalHistory: [],   // every goal, newest first, with computed status
  days: null,       // trailing window; null = all
  view: "chart",
  series: [],       // what's currently plotted
  prefs: null,
};

/* Defaults for a fresh database. Anything added here is picked up by the
   settings drawer automatically — the server stores the blob as-is. */
const DEFAULT_PREFS = { range: 7, view: "chart", theme: "system", dots: true, sync_hours: 6 };

/* ── theme ───────────────────────────────────────────────────── */
/* Three states: an explicit light or dark, or "system" meaning follow the
   OS. Read from localStorage first so there's no flash of the wrong theme
   before prefs arrive from the server. */
function initTheme() {
  let saved = null;
  try { saved = localStorage.getItem("t-fit-theme"); } catch {}
  setTheme(saved || "system");
  $("#btn-theme").addEventListener("click", () => {
    const now = document.documentElement.dataset.theme === "dark" ? "light" : "dark";
    savePrefs({ theme: now });
  });
  // Follow the OS live, but only while set to "system".
  window.matchMedia("(prefers-color-scheme: light)").addEventListener("change", () => {
    if (!state.prefs || state.prefs.theme === "system") {
      setTheme("system");
      draw();
    }
  });
}

function setTheme(pref) {
  const t = pref === "system"
    ? (window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark")
    : pref;
  document.documentElement.dataset.theme = t;
  document.body.dataset.palette = t === "dark" ? "#4d93e8,#e06a3a" : "#2a78d6,#eb6834";
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.content = t === "dark" ? "#1e2124" : "#f6f7f9";
  try { localStorage.setItem("t-fit-theme", pref); } catch {}
}

/* ── formatting ──────────────────────────────────────────────── */
const lb = (n) => (n == null ? "—" : n.toFixed(1));
const signed = (n, d = 1) => (n == null ? "—" : (n > 0 ? "+" : n < 0 ? "−" : "") + Math.abs(n).toFixed(d));
const MON = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
function parseDay(s) { const [y, m, d] = s.split("-").map(Number); return new Date(y, m - 1, d); }
function fmtDay(s) { const d = parseDay(s); return `${MON[d.getMonth()]} ${d.getDate()}, ${d.getFullYear()}`; }
function fmtShort(s) { const d = parseDay(s); return `${MON[d.getMonth()]} ${d.getDate()}`; }
function daysBetween(a, b) { return Math.round((parseDay(b) - parseDay(a)) / 86400000); }
function todayISO() {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}
/** Loss is good, so a negative delta gets the positive color. */
function deltaClass(n) { return n == null || n === 0 ? "" : n < 0 ? "down" : "up"; }

/* ── load ────────────────────────────────────────────────────── */
async function load() {
  const [entries, stats, goals] = await Promise.all([
    api("/api/entries"),
    api("/api/stats"),
    api("/api/goals"),
  ]);
  state.entries = entries;
  state.stats = stats;
  state.goal = stats.goal;
  state.goalHistory = goals;
  renderTiles();
  renderGoalForm();
  renderGoalHistory();
  applyRange();
  renderTable();
  $("#foot-count").textContent =
    entries.length ? `${entries.length.toLocaleString()} entries since ${fmtDay(entries[0].date)}` : "no entries yet";
}

/* ── 7-day trend, computed here so the chart and tiles agree ─── */
function withTrend(rows) {
  const out = [];
  let lo = 0;
  for (let i = 0; i < rows.length; i++) {
    while (daysBetween(rows[lo].date, rows[i].date) > 6) lo++;
    let sum = 0;
    for (let j = lo; j <= i; j++) sum += rows[j].weight_lb;
    out.push({ ...rows[i], trend: sum / (i - lo + 1) });
  }
  return out;
}

function applyRange() {
  let rows = state.entries;
  if (state.days && rows.length) {
    const last = parseDay(rows[rows.length - 1].date);
    const cutoff = new Date(last.getTime() - state.days * 86400000);
    rows = rows.filter((e) => parseDay(e.date) >= cutoff);
  }
  state.series = withTrend(rows);
  const sub = $("#chart-sub");
  sub.textContent = state.series.length
    ? `${state.series.length.toLocaleString()} weigh-ins · ${fmtDay(state.series[0].date)} → ${fmtDay(state.series[state.series.length - 1].date)}`
    : "no data in this range";
  draw();
}

/* ── tiles ───────────────────────────────────────────────────── */
function renderTiles() {
  const s = state.stats;
  if (!s || !s.count) return;

  $("#s-current").textContent = lb(s.current);
  $("#s-current-sub").textContent = s.last ? fmtDay(s.last) : "";

  $("#s-trend").textContent = lb(s.trend_now);
  $("#s-trend-sub").textContent = "7-day average";

  const d30 = $("#s-30");
  d30.textContent = signed(s.change_30d);
  d30.className = "tile-value " + deltaClass(s.change_30d);
  $("#s-30-sub").textContent = s.change_365d != null ? `${signed(s.change_365d)} over a year` : "change";

  const rate = $("#s-rate");
  rate.textContent = signed(s.rate_lb_per_week, 2);
  rate.className = "tile-value " + deltaClass(s.rate_lb_per_week);

  const g = $("#s-goal");
  if (s.goal && s.goal.target_lb != null && s.current != null) {
    const left = s.current - s.goal.target_lb;
    g.textContent = left <= 0 ? "reached" : lb(left);
    g.className = "tile-value" + (left <= 0 ? " down" : "");
    $("#s-goal-sub").textContent =
      left <= 0
        ? `${lb(-left)} lb past ${lb(s.goal.target_lb)}`
        : paceSubtitle(s, left);
  } else {
    g.textContent = "—";
    g.className = "tile-value";
    $("#s-goal-sub").textContent = "set a goal below";
  }
}

/* ── the goal, as a path rather than a floor ──────────────────── */
/**
 * Where the goal says you should be on a given day: a straight line from
 * (start_date, start_lb) to (target_date, target_lb), then flat at the
 * target afterwards. Your trend sitting above or below this is the whole
 * point — a horizontal line at the target can't tell you that.
 *
 * Returns null unless the goal has both ends defined.
 */
function paceAt(iso) {
  const g = state.goal;
  if (!g || g.target_lb == null || !g.target_date || !g.start_date || g.start_lb == null)
    return null;
  const span = daysBetween(g.start_date, g.target_date);
  if (span <= 0) return null;
  const t = daysBetween(g.start_date, iso);
  if (t <= 0) return g.start_lb;
  if (t >= span) return g.target_lb;
  return g.start_lb + (g.target_lb - g.start_lb) * (t / span);
}

/** Ahead of pace (negative) or behind it (positive), in lb. */
function vsPace() {
  const s = state.stats;
  if (!s || !s.last) return null;
  const want = paceAt(s.last);
  const have = s.trend_now ?? s.current;
  return want == null || have == null ? null : have - want;
}

/** The most useful thing the goal can tell you, in one line. */
function paceSubtitle(s, left) {
  const gap = vsPace();
  if (gap != null && Math.abs(gap) >= 0.1) {
    const word = gap < 0 ? "ahead of" : "behind";
    return `lb to go · ${lb(Math.abs(gap))} lb ${word} pace`;
  }
  if (gap != null) return "lb to go · right on pace";
  return s.goal_eta ? `lb to go · on pace for ${fmtDay(s.goal_eta)}`
                    : `lb to go · target ${lb(s.goal.target_lb)}`;
}

/* ── chart ───────────────────────────────────────────────────── */
const SVG = "http://www.w3.org/2000/svg";
const el = (n, a = {}) => {
  const e = document.createElementNS(SVG, n);
  for (const k in a) if (a[k] != null) e.setAttribute(k, a[k]);
  return e;
};

let scale = null; // { x(date)->px, yInv(px)->lb, rows }

function niceTicks(min, max, count) {
  const span = max - min || 1;
  const raw = span / count;
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) || 10 * mag;
  const out = [];
  for (let v = Math.ceil(min / step) * step; v <= max + 1e-9; v += step) out.push(+v.toFixed(6));
  return out;
}

function draw() {
  const svg = $("#chart");
  svg.innerHTML = "";
  const rows = state.series;
  $("#chart-empty").hidden = rows.length > 0;
  if (!rows.length) { scale = null; return; }

  const W = svg.clientWidth || 900;
  const H = svg.clientHeight || 340;
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  const M = { t: 14, r: 16, b: 26, l: 44 };
  const iw = W - M.l - M.r, ih = H - M.t - M.b;

  const t0 = parseDay(rows[0].date).getTime();
  const t1 = parseDay(rows[rows.length - 1].date).getTime();
  const tSpan = t1 - t0 || 1;
  const X = (iso) => M.l + ((parseDay(iso).getTime() - t0) / tSpan) * iw;

  const goalW = state.goal && state.goal.target_lb;
  let lo = Math.min(...rows.map((r) => r.weight_lb));
  let hi = Math.max(...rows.map((r) => r.weight_lb));
  if (goalW != null && goalW >= lo - 40 && goalW <= hi + 40) { lo = Math.min(lo, goalW); hi = Math.max(hi, goalW); }

  // The pace line has to fit on the chart too, or it silently runs off.
  const firstISO = rows[0].date, lastISO = rows[rows.length - 1].date;
  const paceEnds = [paceAt(firstISO), paceAt(lastISO)].filter((v) => v != null);
  for (const v of paceEnds) { lo = Math.min(lo, v); hi = Math.max(hi, v); }
  const pad = Math.max((hi - lo) * 0.09, 1.5);
  lo -= pad; hi += pad;
  const Y = (w) => M.t + ih - ((w - lo) / (hi - lo)) * ih;

  // grid + y axis
  for (const v of niceTicks(lo, hi, 5)) {
    const y = Y(v);
    svg.appendChild(el("line", { class: "grid-line", x1: M.l, x2: W - M.r, y1: y, y2: y }));
    const t = el("text", { class: "axis-text", x: M.l - 8, y: y + 4, "text-anchor": "end" });
    t.textContent = v.toFixed(0);
    svg.appendChild(t);
  }

  // x axis — spaced by *time*, not by index, so dense stretches of data
  // don't pull all the labels into a pile. Anything that would still
  // collide with its neighbour is dropped rather than drawn on top of it.
  {
    const spanDays = daysBetween(rows[0].date, rows[rows.length - 1].date);
    const long = spanDays > 400;
    const nx = Math.max(2, Math.min(7, Math.floor(iw / 120)));
    const half = long ? 34 : 28; // half the width a label needs, in px
    let lastRight = -Infinity;
    for (let i = 0; i < nx; i++) {
      const f = i / (nx - 1);
      const ms = t0 + f * tSpan;
      const dt = new Date(ms);
      const x = M.l + f * iw;
      const anchor = i === 0 ? "start" : i === nx - 1 ? "end" : "middle";
      const left = anchor === "start" ? x : anchor === "end" ? x - half * 2 : x - half;
      if (left < lastRight + 12) continue;
      lastRight = left + half * 2;
      const t = el("text", { class: "axis-text", x, y: H - 8, "text-anchor": anchor });
      t.textContent = long
        ? `${MON[dt.getMonth()]} ${dt.getFullYear()}`
        : `${MON[dt.getMonth()]} ${dt.getDate()}`;
      svg.appendChild(t);
    }
  }

  // goal reference line
  const lgGoal = $("#lg-goal");
  if (goalW != null && goalW > lo && goalW < hi) {
    const y = Y(goalW);
    svg.appendChild(el("line", { class: "goal-line", x1: M.l, x2: W - M.r, y1: y, y2: y }));
    const t = el("text", { class: "goal-label", x: W - M.r, y: y - 7, "text-anchor": "end" });
    t.textContent = `Goal ${lb(goalW)}`;
    svg.appendChild(t);
    lgGoal.hidden = false;
  } else {
    // No line drawn — the goal is off this range's scale — so don't claim
    // one in the legend.
    lgGoal.hidden = true;
  }

  // raw weigh-ins, recessive — the trend line is the signal
  const r = rows.length > 900 ? 1.6 : rows.length > 300 ? 2.1 : 3;
  const showDots = !state.prefs || state.prefs.dots !== false;
  for (const p of showDots ? rows : []) {
    svg.appendChild(el("circle", {
      class: "dot" + (p.memo ? " has-note" : ""),
      cx: X(p.date), cy: Y(p.weight_lb), r: p.memo ? r + 0.8 : r,
    }));
  }

  // The goal, drawn as the path it actually implies. Where the blue trend
  // sits against this orange line is the answer to "am I on track?".
  const lgPace = $("#lg-pace");
  if (paceEnds.length === 2) {
    const pts = [firstISO, lastISO];
    // A vertex at the target date, where the line flattens out.
    const gt = state.goal.target_date;
    if (daysBetween(firstISO, gt) > 0 && daysBetween(gt, lastISO) > 0) pts.splice(1, 0, gt);
    const dPace = pts
      .map((iso, i) => `${i ? "L" : "M"}${X(iso).toFixed(1)} ${Y(paceAt(iso)).toFixed(1)}`)
      .join("");
    svg.appendChild(el("path", { class: "pace-line", d: dPace }));
    lgPace.hidden = false;
  } else {
    lgPace.hidden = true;
  }

  // Past goals, as small markers — what you hit, and what you didn't. The
  // current goal already gets the pace/target lines above, so it's skipped
  // here; only history needs a marker.
  for (const g of state.goalHistory || []) {
    if (g.current) continue;
    let atISO, cls, title;
    if (g.status === "achieved" && g.hit_date) {
      atISO = g.hit_date;
      cls = "goal-marker hit";
      title = `Achieved ${lb(g.target_lb)} lb on ${fmtDay(g.hit_date)}`;
    } else if (g.target_date && (g.status === "missed" || g.status === "superseded")) {
      atISO = g.target_date;
      cls = "goal-marker " + (g.status === "missed" ? "missed" : "superseded");
      title = `${g.status === "missed" ? "Missed" : "Superseded"}: ${lb(g.target_lb)} lb by ${fmtDay(g.target_date)}`;
    } else {
      continue;
    }
    if (daysBetween(firstISO, atISO) < 0 || daysBetween(atISO, lastISO) < 0) continue;
    const my = Y(g.target_lb);
    if (my < M.t || my > M.t + ih) continue;
    const mk = el("circle", { class: cls, cx: X(atISO).toFixed(1), cy: my.toFixed(1), r: 5 });
    const ttl = document.createElementNS(SVG, "title");
    ttl.textContent = title;
    mk.appendChild(ttl);
    svg.appendChild(mk);
  }

  // 7-day trend. The line runs unbroken end to end; a stretch with no
  // weigh-ins for over three weeks is bridged with a dashed segment, so it
  // stays connected but doesn't pretend those weeks were measured.
  const GAP = 21;
  let dSolid = "", dGap = "", pen = false, gaps = 0;
  for (let i = 0; i < rows.length; i++) {
    const x = X(rows[i].date).toFixed(1), y = Y(rows[i].trend).toFixed(1);
    if (i > 0 && daysBetween(rows[i - 1].date, rows[i].date) > GAP) {
      dGap += `M${X(rows[i - 1].date).toFixed(1)} ${Y(rows[i - 1].trend).toFixed(1)}L${x} ${y}`;
      pen = false;
      gaps++;
    }
    dSolid += (pen ? "L" : "M") + x + " " + y + " ";
    pen = true;
  }
  if (dGap) svg.appendChild(el("path", { class: "trend-gap", d: dGap }));
  svg.appendChild(el("path", { class: "trend-line", d: dSolid }));
  $("#lg-gap").hidden = gaps === 0;

  // interaction layer
  const cross = el("line", { class: "crosshair", y1: M.t, y2: M.t + ih, opacity: 0 });
  const focus = el("circle", { class: "focus-dot", r: 5, opacity: 0 });
  svg.appendChild(cross);
  svg.appendChild(focus);
  svg.appendChild(el("rect", { x: M.l, y: M.t, width: iw, height: ih, fill: "transparent", "data-hit": "1" }));

  scale = { X, Y, rows, cross, focus, M, iw, ih, W };
}

function nearestRow(px) {
  const { rows, X } = scale;
  let best = 0, bd = Infinity;
  for (let i = 0; i < rows.length; i++) {
    const d = Math.abs(X(rows[i].date) - px);
    if (d < bd) { bd = d; best = i; }
  }
  return rows[best];
}

function hover(evt) {
  if (!scale) return;
  const svg = $("#chart");
  const box = svg.getBoundingClientRect();
  const px = ((evt.clientX - box.left) / box.width) * scale.W;
  if (px < scale.M.l - 10 || px > scale.M.l + scale.iw + 10) return hoverOff();

  const p = nearestRow(px);
  const x = scale.X(p.date), y = scale.Y(p.weight_lb);
  scale.cross.setAttribute("x1", x); scale.cross.setAttribute("x2", x); scale.cross.setAttribute("opacity", 1);
  scale.focus.setAttribute("cx", x); scale.focus.setAttribute("cy", y); scale.focus.setAttribute("opacity", 1);

  const tip = $("#tip");
  tip.innerHTML =
    `<div class="tt-d">${fmtDay(p.date)}</div>` +
    `<div class="tt-w">${lb(p.weight_lb)} lb</div>` +
    `<div class="tt-t">7-day trend ${lb(p.trend)}</div>` +
    (p.memo ? `<div class="tt-m">${escapeHtml(p.memo)}</div>` : "");
  tip.hidden = false;
  const wrap = $(".chart-wrap").getBoundingClientRect();
  const cx = (x / scale.W) * wrap.width;
  tip.style.left = Math.min(Math.max(cx, 90), wrap.width - 90) + "px";
  tip.style.top = (y / (svg.clientHeight || 340)) * wrap.height + "px";
}
function hoverOff() {
  $("#tip").hidden = true;
  if (scale) { scale.cross.setAttribute("opacity", 0); scale.focus.setAttribute("opacity", 0); }
}
function escapeHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}

/* ── table ───────────────────────────────────────────────────── */
const TABLE_CAP = 400;
function renderTable() {
  const rows = state.entries.slice().reverse();
  const shown = rows.slice(0, TABLE_CAP);
  const body = $("#tbl tbody");
  body.innerHTML = "";
  for (let i = 0; i < shown.length; i++) {
    const e = shown[i];
    const prev = shown[i + 1];
    const delta = prev ? e.weight_lb - prev.weight_lb : null;
    const tr = document.createElement("tr");
    const memo = e.memo || "";
    tr.innerHTML =
      `<td class="day">${fmtDay(e.date)}</td>` +
      `<td class="num">${e.weight_lb.toFixed(1)}</td>` +
      `<td class="num ${deltaClass(delta)}">${delta == null ? "" : signed(delta)}</td>` +
      `<td class="memo${memo ? " has" : ""}"${memo ? ' tabindex="0" role="button" title="Show the whole note"' : ""}>` +
        `<span>${escapeHtml(memo)}</span></td>` +
      `<td class="num"><button class="row-del" title="Delete this entry" aria-label="Delete ${e.date}">×</button></td>`;
    tr.querySelector(".row-del").addEventListener("click", () => removeEntry(e.date));
    // Notes stay on one line until you ask for the rest — a long note
    // shouldn't push the date column into wrapping.
    const cell = tr.querySelector(".memo.has");
    if (cell) {
      const toggle = () => cell.classList.toggle("open");
      cell.addEventListener("click", toggle);
      cell.addEventListener("keydown", (ev) => {
        if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); toggle(); }
      });
    }
    body.appendChild(tr);
  }
  const more = $("#tbl-more");
  more.hidden = rows.length <= TABLE_CAP;
  more.textContent = `Showing the ${TABLE_CAP} most recent of ${rows.length.toLocaleString()} — export the CSV for everything.`;
}

async function removeEntry(date) {
  await api("/api/entries/" + date, { method: "DELETE" });
  await load();
}

/* ── goal ────────────────────────────────────────────────────── */
function renderGoalForm() {
  $("#g-target").value = state.goal?.target_lb ?? "";
  $("#g-date").value = state.goal?.target_date ?? "";
}

/** Refetch stats + goal history and redraw everything that depends on
 * them. Used after any add/edit/delete so the tiles, chart and history
 * list never fall out of sync with each other. */
async function reloadGoals() {
  const [stats, goals] = await Promise.all([api("/api/stats"), api("/api/goals")]);
  state.stats = stats;
  state.goal = stats.goal;
  state.goalHistory = goals;
  renderTiles();
  renderGoalForm();
  renderGoalHistory();
  draw();
}

const STATUS_LABEL = {
  active: ["Active", ""],
  achieved: ["Achieved", "ok"],
  missed: ["Missed", "bad"],
  superseded: ["Superseded", "dim"],
};

function goalRowText(g) {
  const target = `${lb(g.target_lb)} lb`;
  const by = g.target_date ? `by ${fmtShort(g.target_date)}` : "no deadline";
  let outcome = "";
  if (g.status === "achieved" && g.hit_date) {
    const early = g.target_date ? daysBetween(g.hit_date, g.target_date) : null;
    outcome =
      early == null ? ` — hit ${fmtShort(g.hit_date)}`
      : early >= 0 ? ` — hit ${fmtShort(g.hit_date)}, ${early}d early`
      : ` — hit ${fmtShort(g.hit_date)}, ${-early}d late`;
  }
  return `${target} ${by}${outcome}`;
}

/** One history row, rendered fresh each time — no per-row state to patch
 * up, which keeps edit/cancel/delete trivial: just re-render. */
function goalRow(g) {
  const row = document.createElement("div");
  row.className = "goal-row";
  const [label, cls] = STATUS_LABEL[g.status] || [g.status, ""];
  row.innerHTML =
    `<div class="goal-row-main">` +
      `<span class="pill${cls ? " " + cls : ""}">${label}</span>` +
      `<span class="goal-row-text">${goalRowText(g)}</span>` +
    `</div>` +
    `<div class="goal-row-actions">` +
      `<button class="link-btn g-edit" type="button">Edit</button>` +
      `<button class="link-btn g-del" type="button">Delete</button>` +
    `</div>` +
    `<div class="goal-row-sub">from ${lb(g.start_lb)} lb on ${fmtShort(g.start_date)}</div>`;
  row.querySelector(".g-edit").addEventListener("click", () => editGoalRow(row, g));
  row.querySelector(".g-del").addEventListener("click", async () => {
    await api(`/api/goals/${g.id}`, { method: "DELETE" });
    await reloadGoals();
  });
  return row;
}

function editGoalRow(row, g) {
  row.innerHTML = `
    <form class="goal-edit-form">
      <div class="field">
        <label>Target <span class="unit">lb</span></label>
        <input type="number" step="0.1" min="1" max="1500" inputmode="decimal" class="ge-target" value="${g.target_lb}">
      </div>
      <div class="field">
        <label>By <span class="opt">optional</span></label>
        <input type="date" class="ge-date" value="${g.target_date || ""}">
      </div>
      <button type="submit" class="ghost">Save</button>
      <button type="button" class="ghost ge-cancel">Cancel</button>
    </form>`;
  row.querySelector(".goal-edit-form").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const target_lb = parseFloat(row.querySelector(".ge-target").value);
    if (!Number.isFinite(target_lb)) return;
    await api(`/api/goals/${g.id}`, {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        target_lb,
        target_date: row.querySelector(".ge-date").value || null,
        start_lb: g.start_lb,
        start_date: g.start_date,
      }),
    });
    await reloadGoals();
  });
  row.querySelector(".ge-cancel").addEventListener("click", renderGoalHistory);
}

function renderGoalHistory() {
  const wrap = $("#goal-history");
  const list = state.goalHistory || [];
  wrap.hidden = list.length === 0;
  wrap.innerHTML = "";
  for (const g of list) wrap.appendChild(goalRow(g));
}

/* ── wiring ──────────────────────────────────────────────────── */
async function syncNow() {
  const b = $("#w-sync");
  b.disabled = true;
  msg("#w-msg", "Asking Withings…");
  try {
    const r = await api("/api/withings/sync", { method: "POST" });
    await load();
    await loadWithings();
    msg("#w-msg",
      `Added ${r.inserted} new day${r.inserted === 1 ? "" : "s"} from ${r.fetched} readings` +
      (r.skipped_existing ? ` — ${r.skipped_existing} days were already logged` : "") + ".",
      "ok");
  } catch (e) {
    await loadWithings();
    msg("#w-msg", e.message, "err");
  } finally {
    b.disabled = false;
  }
}

function msg(sel, text, kind) {
  const n = $(sel);
  n.textContent = text;
  n.className = "form-msg" + (kind ? " " + kind : "");
  if (kind === "ok") setTimeout(() => { if (n.textContent === text) n.textContent = ""; }, 4000);
}

function wire() {
  $("#f-date").value = todayISO();

  $("#entry-form").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const btn = $("#f-submit");
    btn.disabled = true;
    try {
      await api("/api/entries", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          date: $("#f-date").value,
          weight_lb: parseFloat($("#f-weight").value),
          memo: $("#f-memo").value.trim(),
        }),
      });
      $("#f-weight").value = ""; $("#f-memo").value = "";
      await load();
      msg("#entry-msg", "Logged.", "ok");
    } catch (e) {
      msg("#entry-msg", e.message, "err");
    } finally {
      btn.disabled = false;
    }
  });

  $("#goal-form").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    const target_lb = parseFloat($("#g-target").value);
    if (!Number.isFinite(target_lb)) {
      msg("#goal-msg", "Enter a target weight.", "err");
      return;
    }
    try {
      // Always a fresh goal: start is wherever you are right now, and
      // whatever was current before drops into history automatically.
      await api("/api/goals", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          target_lb,
          target_date: $("#g-date").value || null,
        }),
      });
      await reloadGoals();
      msg("#goal-msg", "Saved.", "ok");
    } catch (e) {
      msg("#goal-msg", e.message, "err");
    }
  });

  $("#imp-file").addEventListener("change", async (ev) => {
    const f = ev.target.files[0];
    if (!f) return;
    msg("#imp-msg", `Reading ${f.name}…`);
    try {
      const rep = await api("/api/import?source=import", {
        method: "POST",
        headers: { "content-type": "text/csv" },
        body: await f.text(),
      });
      await load();
      msg("#imp-msg",
        `Added ${rep.inserted} of ${rep.read} rows — ${rep.skipped_existing} days already logged` +
        (rep.errors.length ? `, ${rep.errors.length} rows skipped` : "") + ".",
        "ok");
    } catch (e) {
      msg("#imp-msg", e.message, "err");
    } finally {
      ev.target.value = "";
    }
  });

  document.querySelectorAll("[data-days]").forEach((b) =>
    b.addEventListener("click", () => {
      document.querySelectorAll("[data-days]").forEach((o) => o.classList.remove("on"));
      b.classList.add("on");
      state.days = b.dataset.days ? +b.dataset.days : null;
      applyRange();
    }));

  document.querySelectorAll("[data-view]").forEach((b) =>
    b.addEventListener("click", () => {
      document.querySelectorAll("[data-view]").forEach((o) => o.classList.remove("on"));
      b.classList.add("on");
      state.view = b.dataset.view;
      $("#view-chart").hidden = state.view !== "chart";
      $("#view-table").hidden = state.view !== "table";
      if (state.view === "chart") draw();
    }));

  const svg = $("#chart");
  svg.addEventListener("mousemove", hover);
  svg.addEventListener("mouseleave", hoverOff);
  svg.addEventListener("touchmove", (e) => { hover(e.touches[0]); }, { passive: true });
  svg.addEventListener("touchend", hoverOff);

  let rt;
  window.addEventListener("resize", () => { clearTimeout(rt); rt = setTimeout(draw, 120); });

  /* Withings */
  $("#w-form").addEventListener("submit", async (ev) => {
    ev.preventDefault();
    try {
      await api("/api/withings/config", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          client_id: $("#w-id").value.trim(),
          client_secret: $("#w-secret").value.trim(),
        }),
      });
      $("#w-secret").value = "";
      msg("#w-msg", "Saved. Now connect to Withings.", "ok");
      await loadWithings();
    } catch (e) { msg("#w-msg", e.message, "err"); }
  });

  $("#w-link").addEventListener("click", async () => {
    try {
      const { url } = await api("/api/withings/authorize");
      // Always expose the link. An app-mode window can silently swallow
      // window.open, and then nothing at all appears to happen.
      const a = $("#w-authlink");
      a.href = url;
      $("#w-fallback").hidden = false;
      window.open(url, "_blank", "noopener");
      msg("#w-msg", "Waiting for you to approve it at Withings…");
      watchForLink();
    } catch (e) { msg("#w-msg", e.message, "err"); }
  });

  // The approval happens in another tab, so this window has to go looking
  // for the result rather than assuming it worked.
  let watchTimer = null;
  function watchForLink() {
    clearInterval(watchTimer);
    const until = Date.now() + 5 * 60 * 1000;
    watchTimer = setInterval(async () => {
      const linked = await loadWithings();
      if (linked) {
        clearInterval(watchTimer);
        $("#w-fallback").hidden = true;
        msg("#w-msg", "Linked. Pulling your weigh-ins…", "ok");
        await syncNow();
      } else if (Date.now() > until) {
        clearInterval(watchTimer);
        msg("#w-msg", "Gave up waiting. Hit Connect to Withings to try again.", "err");
      }
    }, 2000);
  }

  // The callback tab tells us directly when it can.
  window.addEventListener("message", (ev) => {
    if (ev.data && ev.data.tfit === "withings") loadWithings().then((l) => l && syncNow());
  });

  $("#w-err-x").addEventListener("click", async () => {
    await api("/api/withings/clear_error", { method: "POST" });
    loadWithings();
  });

  $("#w-sync").addEventListener("click", syncNow);

  $("#w-unlink").addEventListener("click", async () => {
    try {
      await api("/api/withings/unlink", { method: "POST" });
      await loadWithings();
      msg("#w-msg", "Unlinked. Your weight history is untouched.", "ok");
    } catch (e) { msg("#w-msg", e.message, "err"); }
  });

  /* Settings drawer */
  const dlg = $("#settings");
  $("#btn-settings").addEventListener("click", () => dlg.showModal());
  // Click outside the panel closes it; <dialog> handles Esc itself.
  dlg.addEventListener("click", (ev) => { if (ev.target === dlg) dlg.close(); });

  $("#p-range").addEventListener("change", (e) =>
    savePrefs({ range: e.target.value === "all" ? "all" : +e.target.value }));
  $("#p-view").addEventListener("change", (e) => savePrefs({ view: e.target.value }));
  $("#p-theme").addEventListener("change", (e) => { savePrefs({ theme: e.target.value }); draw(); });
  $("#p-dots").addEventListener("change", (e) => savePrefs({ dots: e.target.checked }));
  $("#p-sync").addEventListener("change", (e) => savePrefs({ sync_hours: +e.target.value }));

}

/**
 * `#w-sub` lives in Settings, right next to the buttons that act on it, so
 * it just says where things stand — no need to point anywhere else.
 */
async function loadWithings() {
  let w;
  try { w = await api("/api/withings/status"); }
  catch {
    $("#w-sub").textContent = "unavailable";
    return false;
  }

  $("#w-redirect").textContent = w.redirect_uri;

  // Show what's actually stored. The boxes used to come back empty on every
  // run, which looked exactly like nothing had been saved.
  if (w.client_id) $("#w-id").value = w.client_id;
  const sec = $("#w-secret");
  sec.placeholder = w.has_secret ? "•••••••••••• saved" : "…";
  $("#w-saved-hint").hidden = !w.has_secret;
  $("#w-link").hidden = !w.configured || w.linked;
  $("#w-sync").hidden = !w.linked;
  $("#w-unlink").hidden = !w.linked;

  $("#w-err").hidden = !w.last_error;
  if (w.last_error) $("#w-err-text").textContent = w.last_error;

  // Three states, and the card should only ever describe the one you're in.
  const setup = $("#w-setup");
  const summary = $("#w-setup-summary");

  if (w.linked) {
    // Done. Nothing about setting up belongs on screen.
    const when = w.last_sync ? new Date(w.last_sync) : null;
    $("#w-sub").textContent = when
      ? `Connected · last synced ${when.toLocaleString()}`
      : "Connected · not synced yet";
    setup.hidden = true;
  } else if (w.configured) {
    // Registered but not linked. The steps are done, so fold them away —
    // Connect lives in this section's header now, not hidden with them.
    setup.hidden = false;
    setup.open = false;
    summary.textContent = "Registration details — open this to change the client ID or secret";
    $("#w-sub").textContent = "Registered. One step left: Connect to Withings.";
  } else {
    // Nothing saved yet: the steps are the point.
    setup.hidden = false;
    setup.open = true;
    summary.textContent = "Set it up — one time, about five minutes";
    $("#w-sub").textContent = "Not set up. Pull weigh-ins straight off your scale.";
  }
  return !!w.linked;
}

/* ── preferences ─────────────────────────────────────────────── */
async function loadPrefs() {
  let stored = {};
  try { stored = (await api("/api/prefs")) || {}; } catch { /* fall back to defaults */ }
  state.prefs = { ...DEFAULT_PREFS, ...stored };
  syncPrefsForm();
  setTheme(state.prefs.theme);
  applyDefaultRange();
  applyDefaultView();
}

/** Reflect the stored prefs in the settings drawer's controls. */
function syncPrefsForm() {
  const p = state.prefs;
  $("#p-range").value = String(p.range);
  $("#p-view").value = p.view === "table" ? "table" : "chart";
  $("#p-theme").value = p.theme;
  $("#p-dots").checked = p.dots !== false;
  $("#p-sync").value = String(p.sync_hours ?? 6);
}

function applyDefaultRange() {
  const r = state.prefs.range;
  state.days = r === "all" || r === null ? null : Number(r);
  document.querySelectorAll("[data-days]").forEach((b) => {
    const v = b.dataset.days ? +b.dataset.days : null;
    b.classList.toggle("on", v === state.days);
  });
}

function applyDefaultView() {
  state.view = state.prefs.view === "table" ? "table" : "chart";
  document.querySelectorAll("[data-view]").forEach((b) =>
    b.classList.toggle("on", b.dataset.view === state.view));
  $("#view-chart").hidden = state.view !== "chart";
  $("#view-table").hidden = state.view !== "table";
}

/**
 * Change settings, apply them, then persist.
 *
 * Only the settings that actually changed take effect on the current view —
 * otherwise flipping the theme would yank the chart back to the default
 * range while you were looking at something else.
 */
async function savePrefs(patch) {
  state.prefs = { ...(state.prefs || DEFAULT_PREFS), ...patch };
  syncPrefsForm();

  if ("theme" in patch) setTheme(state.prefs.theme);
  if ("range" in patch) applyDefaultRange();
  if ("view" in patch) applyDefaultView();
  applyRange();

  try {
    await api("/api/prefs", {
      method: "PUT",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(state.prefs),
    });
  } catch (e) {
    console.warn("could not save settings:", e.message);
  }
}

async function loadVersion() {
  try {
    const v = await api("/api/version");
    $("#foot-build").textContent = `v${v.version} · built ${v.built}`;
    if (v.db_path) $("#p-dbpath").textContent = v.db_path;
  } catch { /* older build without the endpoint */ }
}

initTheme();
wire();
boot();

/* Prefs before data: the first render should already be the range you chose,
   rather than snapping to it a moment later. */
async function boot() {
  await loadPrefs();
  try {
    await load();
  } catch (e) {
    msg("#entry-msg", "Could not load data: " + e.message, "err");
  }
  loadWithings();
  loadVersion();
}
