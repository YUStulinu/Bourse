"""Render results/*.json as results/README.md."""

from __future__ import annotations

import json
import os

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
R = os.path.join(ROOT, "results")


def load(name):
    p = os.path.join(R, name)
    return json.load(open(p)) if os.path.exists(p) else None


def f(x, d=1):
    if x is None:
        return "–"
    if isinstance(x, float):
        return f"{x:,.{d}f}"
    return f"{x:,}"


def itch_section(out):
    ref, arr, go = load("itch_replay.json"), load("itch_replay_arrival.json"), load("go_itchcheck.json")
    if not ref:
        return
    out.append("## Rebuilding the real order books (NASDAQ TotalView-ITCH 5.0, 2019-01-30)\n")
    out.append("| | C++ (engine's book) | Go (market-data books) |")
    out.append("|---|---:|---:|")
    rows = [
        ("messages", "messages", "messages"),
        ("orders added", "orders_added", "orders_added"),
        ("largest number of live orders", "max_live_orders", None),
        ("live orders at the end of the day", "live_orders_at_end", "live_orders_at_end"),
        ("messages naming an unknown order", "unknown_order_references", "unknown_order_references"),
        ("executions or cancels over the open quantity", "executions_or_cancels_over_open_quantity", "executions_or_cancels_over"),
        ("book checks in regular hours", "book_checks_in_regular_hours", "book_checks"),
        ("crossed books", "crossed", "crossed"),
        ("locked books", "locked", "locked"),
        ("shares executed against displayed orders", "executed_shares", "executed_shares"),
        ("shares executed against hidden orders", "hidden_trade_shares", "hidden_trade_shares"),
        ("seconds", "seconds", "seconds"),
        ("messages per second", "messages_per_second", "messages_per_second"),
    ]
    for name, k1, k2 in rows:
        out.append(f"| {name} | {f(ref.get(k1), 0)} | {f(go.get(k2), 0) if go and k2 else '–'} |")
    out.append("")
    out.append("Matching priority: consecutive executions with the same timestamp against one side of one stock come from one incoming order. "
               "For each such group in regular trading, the engine was asked which resting orders an order of that size and limit would hit.\n")
    out.append("| priority within a price level | groups checked | same orders and quantities as NASDAQ | different |")
    out.append("|---|---:|---:|---:|")
    for name, r in (("by order reference number (entry order)", ref), ("by arrival of the add message", arr)):
        if r:
            out.append(f"| {name} | {f(r['groups_checked'], 0)} | {f(r['groups_same_as_engine'], 0)} | {f(r['groups_different'], 0)} |")
    out.append("")


def engine_rows(b):
    e = b.get("engine")
    if not e:
        return []
    rows = [("in process", e["in_process"]["commands_per_second"], e["in_process"]["latency_ns"], "ns")]
    for r in e["bus"]:
        rows.append((f"bus, {f(r['rate'] / 1e6, 1)} M/s offered" if r["rate"] else "bus, as fast as possible", r["achieved_per_second"], r["latency_ns"], "ns"))
    return rows


def bench_section(out):
    for plat in ("windows", "linux"):
        b = load(f"bench_{plat}.json")
        if not b:
            continue
        out.append(f"## Latency and throughput: {b['platform']}\n")
        out.append("### Matching engine\n")
        out.append("| | commands/s | p50 | p90 | p99 | p99.9 | p99.99 | max |")
        out.append("|---|---:|---:|---:|---:|---:|---:|---:|")
        for name, rate, lat, unit in engine_rows(b):
            out.append(f"| {name} | {f(rate / 1e6, 2)} M | " + " | ".join(f"{f(lat[k] / 1000, 2)} µs" for k in ("p50", "p90", "p99", "p99_9", "p99_99", "max")) + " |")
        out.append("")
        t = b["tcp_baseline"]["tcp_loopback_rtt_us"]
        out.append("### Order entry over TCP\n")
        out.append(f"Plain TCP round trip on loopback (49 bytes, echo): p50 {f(t['p50'])} µs, p99 {f(t['p99'])} µs, p99.9 {f(t['p99_9'])} µs.\n")
        out.append("| protocol | orders/s | round trip p50 | p99 | p99.9 | inside the gateway p50 | p99 | p99.9 |")
        out.append("|---|---:|---:|---:|---:|---:|---:|---:|")
        for r in b["order_entry"]:
            l, g = r["latency_us"], r["gateway_internal_us"]
            out.append(f"| {r['protocol'].upper()} | {f(r['rate'], 0)} | {f(l['p50'])} | {f(l['p99'])} | {f(l['p99_9'])} | {f(g['p50'])} | {f(g['p99'])} | {f(g['p99_9'])} |")
        out.append("")
        out.append("### Market data fan-out (MoldUDP64, 20,000 orders/s flowing)\n")
        out.append("| subscribers | dropped on purpose | messages/s delivered | latency p50 | p99 | p99.9 | gaps | recovered | missing |")
        out.append("|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
        for r in b["market_data"]:
            l = r["latency_us"]
            out.append(f"| {r['subscribers']} | {f(r['drop'] * 100, 0)}% | {f(r['messages_per_second_all_subscribers'], 0)} | {f(l['p50'])} µs | {f(l['p99'])} µs | {f(l['p99_9'])} µs | "
                       f"{f(r['gaps'], 0)} | {f(r['recovered'], 0)} | {f(r['still_missing'], 0)} |")
        out.append("")


def realism_section(out):
    d = load("realism.json")
    if not d:
        return
    cols = list(d["real"]) + list(d["sim"])
    out.append("## Statistical realism of the simulated market\n")
    out.append("| fact | " + " | ".join(cols) + " |")
    out.append("|---|" + "---|" * len(cols))
    for fact in d["checks"][cols[0]]:
        out.append(f"| {fact} | " + " | ".join("yes" if d["checks"][c][fact] else "**no**" for c in cols) + " |")
    out.append("")
    stats = [
        ("trades per minute", "trades_per_minute", 0),
        ("volatility over 10 s (ticks)", "volatility_10s_ticks", 2),
        ("kurtosis of 10 s returns", "kurtosis_10s", 1),
        ("Hill tail exponent of 10 s returns", "hill_10s", 2),
        ("autocorrelation of |10 s returns|, lags 1-30", "abs_return_acf_mean_1_30", 3),
        ("|autocorrelation| of 10 s returns, lags 1-30", "return_acf_mean_abs_1_30", 3),
        ("decay exponent of the trade-sign autocorrelation", "sign_acf_exponent", 2),
        ("trade-sign autocorrelation at lag 1", "sign_acf_1", 2),
        ("mean spread (ticks)", "spread_mean_ticks", 2),
        ("impact of a trade after 10 s (ticks)", "impact_mean_ticks", 2),
        ("coefficient of variation of inter-trade times", "interarrival_cv", 2),
        ("limit orders at the best price or inside", "add_at_best_or_inside", 2),
        ("tail exponent of limit-order distances", "add_distance_tail_exponent", 2),
    ]
    out.append("| statistic | " + " | ".join(cols) + " |")
    out.append("|---|" + "---:|" * len(cols))
    allv = {**d["real"], **d["sim"]}
    for name, k, dd in stats:
        out.append(f"| {name} | " + " | ".join(f(allv[c].get(k), dd) for c in cols) + " |")
    ks = {c: d["sim"][c].get("ks_10s_returns_vs_real") for c in d["sim"]}
    out.append("")
    out.append("Kolmogorov-Smirnov distance between normalized 10 s returns: " + ", ".join(f"{c} vs the five real stocks {f(v, 3)}" for c, v in ks.items()) +
               "; each real stock against the four others: " + ", ".join(f"{s} {f(v, 3)}" for s, v in d["ks_10s_returns_real_stock_vs_rest"].items()) + ".\n")


def mm_section(out):
    d = load("mm.json")
    if not d:
        return
    out.append("## Market makers: learned (PPO) against rules, same seat, unseen days\n")
    out.append("| maker | mean P&L per half hour ($) | std ($) | profitable episodes | mean abs. inventory (shares) | market spread (ticks) |")
    out.append("|---|---:|---:|---:|---:|---:|")
    for k in ("learned (PPO)", "Avellaneda-Stoikov", "fixed, at the touch"):
        r = d.get(k)
        if r:
            out.append(f"| {k} | {f(r['pnl_mean'], 0)} | {f(r['pnl_std'], 0)} | {f(100 * r['share_of_episodes_profitable'], 0)}% | {f(r['mean_abs_inventory'], 0)} | {f(r['market_spread_ticks'], 2)} |")
    out.append("")
    if d.get("learned_action_share"):
        out.append("What the learned policy does (share of decisions): " + ", ".join(f"{k}: {f(100 * v, 0)}%" for k, v in sorted(d["learned_action_share"].items(), key=lambda x: -x[1])[:8]) + ".\n")


def replay_section(out):
    d = load("replay_check.json")
    if not d:
        return
    out.append("## The whole exchange end to end\n")
    out.append(f"A simulated market of {f(d['seconds'], 0)} s ({f(d['commands'], 0)} commands, {f(d['sim_trades'], 0)} trades) replayed through OUCH "
               f"to the running exchange at {f(d['commands_per_second'], 0)} commands/s (each command after the previous one's acknowledgment):\n")
    for k, v in d.items():
        if isinstance(v, bool):
            out.append(f"- {k.replace('_', ' ')}: {'yes' if v else 'NO'}")
    out.append(f"- market data: {d['market_data_unknown_refs']} unknown orders, {d['market_data_crossed']} crossed books; clearing booked {f(d['clearing_trades'], 0)} trades\n")


def main():
    out = ["# Measured results\n", "Produced by the scripts in `eval/`; see the main README for how to run them.\n"]
    itch_section(out)
    bench_section(out)
    replay_section(out)
    realism_section(out)
    mm_section(out)
    cal = load("calibration.json")
    if cal:
        out.append("## Calibration\n")
        out.append(f"Random search over {cal['trials']} parameter sets (two half-hour simulations each): distance to the real statistics "
                   f"{f(cal['default_distance'], 2)} with the hand-set defaults, {f(cal['best_distance'], 2)} after calibration.\n")
        out.append("```\n" + json.dumps({k: round(v, 5) for k, v in cal["params"].items()}, indent=1) + "\n```\n")
    open(os.path.join(R, "README.md"), "w", encoding="utf-8").write("\n".join(out) + "\n")
    print("wrote results/README.md")


if __name__ == "__main__":
    main()
