// Pure functions behind the order book view (tested in test/ui.test.mjs).

export interface Level {
  price: number; // 1/10000 dollar
  shares: number;
  orders: number;
}

export interface Trade {
  ts: number; // ns since midnight
  price: number;
  shares: number;
  side: "B" | "S"; // the aggressor
}

export interface BookMsg {
  type: "book";
  symbol: string;
  bids: Level[] | null;
  asks: Level[] | null;
  last: number;
  volume: number;
  trades: Trade[] | null;
  ts: number;
}

export function formatPrice(p: number): string {
  return (p / 10000).toFixed(2);
}

export function formatShares(n: number): string {
  if (n >= 1e6) return (n / 1e6).toFixed(2) + "M";
  if (n >= 1e4) return (n / 1e3).toFixed(1) + "k";
  return String(n);
}

export function formatTime(nsOfDay: number): string {
  const ms = Math.floor(nsOfDay / 1e6);
  const h = Math.floor(ms / 3600000);
  const m = Math.floor(ms / 60000) % 60;
  const s = Math.floor(ms / 1000) % 60;
  const pad = (x: number, n = 2) => String(x).padStart(n, "0");
  return `${pad(h)}:${pad(m)}:${pad(s)}.${pad(ms % 1000, 3)}`;
}

/** Mid price and spread (null when a side is empty). */
export function midSpread(bids: Level[], asks: Level[]): { mid: number; spread: number } | null {
  if (!bids.length || !asks.length) return null;
  return { mid: (bids[0].price + asks[0].price) / 2, spread: asks[0].price - bids[0].price };
}

/** Running totals of shares from the best level outward. */
export function cumulative(levels: Level[]): number[] {
  const out: number[] = [];
  let s = 0;
  for (const l of levels) {
    s += l.shares;
    out.push(s);
  }
  return out;
}

/** Share of the volume at the best levels that is on the bid side (-1..1 scale: bid - ask over total). */
export function imbalance(bids: Level[], asks: Level[], levels = 5): number {
  const b = bids.slice(0, levels).reduce((s, l) => s + l.shares, 0);
  const a = asks.slice(0, levels).reduce((s, l) => s + l.shares, 0);
  return b + a ? (b - a) / (b + a) : 0;
}

/** Volume-weighted average price of trades. */
export function vwap(trades: Trade[]): number {
  let v = 0;
  let pv = 0;
  for (const t of trades) {
    v += t.shares;
    pv += t.shares * t.price;
  }
  return v ? pv / v : 0;
}

/** Keep the newest `max` items of a list that grows at the front. */
export function prependCapped<T>(list: T[], items: T[], max: number): T[] {
  return [...items.slice().reverse(), ...list].slice(0, max);
}

/** A point series for a price chart: one point per trade, bounded length. */
export class Series {
  points: { ts: number; price: number }[] = [];
  constructor(public max = 2000) {}
  add(trades: Trade[]) {
    for (const t of trades) this.points.push({ ts: t.ts, price: t.price });
    this.trim();
  }
  addPoint(ts: number, price: number) {
    this.points.push({ ts, price });
    this.trim();
  }
  private trim() {
    if (this.points.length > this.max) this.points.splice(0, this.points.length - this.max);
  }
  range(): [number, number] {
    let lo = Infinity;
    let hi = -Infinity;
    for (const p of this.points) {
      lo = Math.min(lo, p.price);
      hi = Math.max(hi, p.price);
    }
    return lo <= hi ? [lo, hi] : [0, 1];
  }
}
