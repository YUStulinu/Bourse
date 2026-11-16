import { test } from "node:test";
import assert from "node:assert/strict";
import { cumulative, formatPrice, formatShares, formatTime, imbalance, midSpread, prependCapped, Series, vwap } from "../dist/book.js";

const L = (price, shares, orders = 1) => ({ price, shares, orders });

test("formatting", () => {
  assert.equal(formatPrice(1543200), "154.32");
  assert.equal(formatShares(950), "950");
  assert.equal(formatShares(25300), "25.3k");
  assert.equal(formatShares(2_500_000), "2.50M");
  assert.equal(formatTime(((14 * 60 + 30) * 60 + 5) * 1e9 + 123e6), "14:30:05.123");
});

test("mid, spread, cumulative depth and imbalance", () => {
  const bids = [L(1000000, 300), L(999900, 500)];
  const asks = [L(1000100, 100), L(1000200, 100)];
  assert.deepEqual(midSpread(bids, asks), { mid: 1000050, spread: 100 });
  assert.equal(midSpread([], asks), null);
  assert.deepEqual(cumulative(bids), [300, 800]);
  assert.equal(imbalance(bids, asks), (800 - 200) / 1000);
  assert.equal(imbalance([], []), 0);
});

test("vwap, tape and series", () => {
  const t = (price, shares) => ({ ts: 0, price, shares, side: "B" });
  assert.equal(vwap([t(100, 1), t(200, 3)]), 175);
  assert.equal(vwap([]), 0);
  // newest first, capped
  assert.deepEqual(prependCapped([3, 2, 1], [4, 5], 4), [5, 4, 3, 2]);
  const s = new Series(3);
  s.add([t(5, 1), t(9, 1), t(7, 1), t(6, 1)]);
  assert.equal(s.points.length, 3);
  assert.deepEqual(s.range(), [6, 9]);
});
