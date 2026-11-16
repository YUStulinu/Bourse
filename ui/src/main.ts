// The live order book: connects to the market-data server's WebSocket,
// shows the depth ladder, the trade tape and a price chart of the chosen
// stock, and the accounts of the risk service.
import { BookMsg, Level, Series, Trade, cumulative, formatPrice, formatShares, formatTime, imbalance, midSpread, prependCapped, vwap } from "./book.js";

interface SymbolInfo {
  symbol: string;
  bid: number;
  ask: number;
  last: number;
  volume: number;
  trades: number;
}

const $ = (id: string) => document.getElementById(id)!;
const LEVELS = 10; // shown per side
let ws: WebSocket | null = null;
let symbol = new URLSearchParams(location.search).get("symbol") || "";
let tape: Trade[] = [];
let series = new Series();
let lastBook: BookMsg | null = null;
const riskUrl = new URLSearchParams(location.search).get("risk") || `${location.protocol}//${location.hostname}:8081`;

function connect() {
  const url = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws${symbol ? "?symbol=" + symbol : ""}`;
  ws = new WebSocket(url);
  ws.onopen = () => setStatus("live", true);
  ws.onclose = () => {
    setStatus("disconnected: retrying", false);
    setTimeout(connect, 1000);
  };
  ws.onmessage = (ev) => {
    const msg = JSON.parse(ev.data);
    if (msg.type === "symbols") renderSymbols(msg.symbols as SymbolInfo[]);
    else if (msg.type === "book" && msg.symbol === symbol) renderBook(msg as BookMsg);
  };
}

function setStatus(text: string, ok: boolean) {
  const el = $("status");
  el.textContent = text;
  el.className = ok ? "status ok" : "status bad";
}

function choose(s: string) {
  if (s === symbol) return;
  symbol = s;
  tape = [];
  series = new Series();
  lastBook = null;
  history.replaceState(null, "", `?symbol=${s}`);
  ws?.send(JSON.stringify({ subscribe: s }));
  $("title").textContent = s;
}

function renderSymbols(list: SymbolInfo[]) {
  if (!symbol && list.length) choose(list[0].symbol);
  const body = list
    .map(
      (s) =>
        `<tr data-s="${s.symbol}" class="${s.symbol === symbol ? "sel" : ""}"><td>${s.symbol}</td><td class="num">${s.bid ? formatPrice(s.bid) : "–"}</td>` +
        `<td class="num">${s.ask ? formatPrice(s.ask) : "–"}</td><td class="num">${s.last ? formatPrice(s.last) : "–"}</td><td class="num">${formatShares(s.volume)}</td></tr>`,
    )
    .join("");
  const t = $("symbols") as HTMLTableSectionElement;
  t.innerHTML = body;
  t.querySelectorAll("tr").forEach((tr) => tr.addEventListener("click", () => choose((tr as HTMLElement).dataset.s!)));
}

function ladderRows(levels: Level[], side: "bid" | "ask", maxCum: number): string {
  const cum = cumulative(levels);
  return levels
    .map((l, i) => {
      const w = maxCum ? (100 * cum[i]) / maxCum : 0;
      return `<tr class="${side}"><td class="bar"><span style="width:${w.toFixed(1)}%"></span></td><td class="num">${l.orders}</td>` +
        `<td class="num">${formatShares(l.shares)}</td><td class="num price">${formatPrice(l.price)}</td></tr>`;
    })
    .join("");
}

function renderBook(b: BookMsg) {
  lastBook = b;
  const bids = (b.bids || []).slice(0, LEVELS);
  const asks = (b.asks || []).slice(0, LEVELS);
  const maxCum = Math.max(cumulative(bids).at(-1) || 0, cumulative(asks).at(-1) || 0);
  $("asks").innerHTML = ladderRows(asks.slice().reverse(), "ask", maxCum);
  $("bids").innerHTML = ladderRows(bids, "bid", maxCum);
  const ms = midSpread(bids, asks);
  $("mid").textContent = ms ? `mid ${formatPrice(ms.mid)} · spread ${formatPrice(ms.spread)}` : "one side empty";
  $("imb").textContent = `imbalance ${(100 * imbalance(bids, asks)).toFixed(0)}%`;
  const trades = b.trades || [];
  if (trades.length) tape = prependCapped(tape, trades, 60);
  if (ms) series.addPoint(b.ts, ms.mid);
  $("tape").innerHTML = tape
    .map((t) => `<tr class="${t.side === "B" ? "up" : "down"}"><td>${formatTime(t.ts)}</td><td class="num">${formatPrice(t.price)}</td><td class="num">${formatShares(t.shares)}</td></tr>`)
    .join("");
  $("stats").textContent = `last ${b.last ? formatPrice(b.last) : "–"} · volume ${formatShares(b.volume)} · VWAP (tape) ${tape.length ? formatPrice(vwap(tape)) : "–"}`;
  drawChart();
}

function drawChart() {
  const c = $("chart") as HTMLCanvasElement;
  const dpr = window.devicePixelRatio || 1;
  const w = c.clientWidth;
  const h = c.clientHeight;
  if (c.width !== w * dpr) {
    c.width = w * dpr;
    c.height = h * dpr;
  }
  const g = c.getContext("2d")!;
  g.setTransform(dpr, 0, 0, dpr, 0, 0);
  g.clearRect(0, 0, w, h);
  const css = getComputedStyle(document.documentElement);
  const pts = series.points;
  if (pts.length < 2) {
    g.fillStyle = css.getPropertyValue("--muted");
    g.fillText("waiting for quotes", 10, 20);
    return;
  }
  const [lo, hi] = series.range();
  const pad = Math.max(200, (hi - lo) * 0.1); // at least two ticks of margin
  const y = (p: number) => h - 18 - ((p - lo + pad) / (hi - lo + 2 * pad)) * (h - 30);
  const x = (i: number) => 4 + (i / (pts.length - 1)) * (w - 60);
  g.strokeStyle = css.getPropertyValue("--grid");
  g.lineWidth = 1;
  g.fillStyle = css.getPropertyValue("--muted");
  g.font = "11px system-ui, sans-serif";
  for (const p of [lo, (lo + hi) / 2, hi]) {
    g.beginPath();
    g.moveTo(0, y(p));
    g.lineTo(w - 56, y(p));
    g.stroke();
    g.fillText(formatPrice(p), w - 52, y(p) + 4);
  }
  g.strokeStyle = css.getPropertyValue("--accent");
  g.lineWidth = 2;
  g.beginPath();
  pts.forEach((p, i) => (i ? g.lineTo(x(i), y(p.price)) : g.moveTo(x(i), y(p.price))));
  g.stroke();
}

async function pollRisk() {
  try {
    const r = await fetch(`${riskUrl}/api/accounts`);
    const accounts = await r.json();
    $("accounts").innerHTML = accounts
      .map(
        (a: any) =>
          `<tr class="${a.blocked ? "blocked" : ""}"><td>${a.account}</td><td class="num">${a.equity.toFixed(0)}</td><td class="num">${a.margin.toFixed(0)}</td>` +
          `<td class="num">${(a.realized + a.unrealized).toFixed(0)}</td><td>${Object.entries(a.positions).map(([s, q]) => `${s} ${q}`).join(", ") || "–"}</td>` +
          `<td>${a.blocked ? "blocked" : ""}</td></tr>`,
      )
      .join("");
    $("risk-status").textContent = "";
  } catch {
    $("risk-status").textContent = "risk service not reachable";
  }
}

window.addEventListener("resize", () => lastBook && drawChart());
connect();
pollRisk();
setInterval(pollRisk, 2000);
