// itch-replay: rebuild every order book of a NASDAQ TotalView-ITCH 5.0 file
// with the matching engine's book, check it, and check the engine's matching
// priority against the real executions.
//
//   itch-replay FILE [--max N] [--export AAPL,MSFT --out DIR] [--json OUT]
//               [--priority reference|arrival] [--trace REF,REF]
//
// Checks while replaying:
//  - every execution, cancel, delete and replace names a live order with
//    enough shares (an error in parsing or in the book shows up here first);
//  - during regular hours, the book of a stock that is trading is never
//    crossed (best bid above best ask) or locked (equal);
//  - matching: consecutive executions with the same timestamp against the
//    same side of one stock come from one incoming order. Before applying
//    them, the engine is asked which resting orders an incoming order of
//    that size and limit would hit (in price-time priority); the answer must
//    be the same orders with the same quantities, in the same order.
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <unordered_map>
#include <vector>

#include "bourse/engine.hpp"
#include "bourse/itch.hpp"

using namespace bourse;
using namespace bourse::itch;

namespace {

struct Stock {
    std::string name;
    char state = 'T';
    std::FILE* events = nullptr;
    std::FILE* trades = nullptr;
    std::FILE* quotes = nullptr;
};

struct Group {
    bool active = false;
    uint16_t loc = 0;
    uint64_t ts = 0;
    uint8_t rest_side = 0;
    bool has_c = false, unknown = false;
    int64_t worst = 0;
    uint64_t qty = 0;
    std::vector<Fill> fills;
};

struct Stats {
    uint64_t messages = 0, by_type[256] = {};
    uint64_t adds = 0, max_live = 0, unknown_ref = 0, over_exec = 0, bad_replace = 0;
    uint64_t cross_checks = 0, crossed = 0, locked = 0;
    uint64_t groups = 0, g_outside = 0, g_with_c = 0, g_unknown = 0, g_checked = 0, g_exact = 0, g_mismatch = 0;
    uint64_t fills_checked = 0;
    uint64_t exec_shares = 0, trade_shares = 0, cross_shares = 0;
};

std::string trim(const uint8_t* p, int n) {
    std::string s(reinterpret_cast<const char*>(p), static_cast<size_t>(n));
    while (!s.empty() && s.back() == ' ') s.pop_back();
    return s;
}

}  // namespace

int main(int argc, char** argv) {
    if (argc < 2) {
        std::fprintf(stderr, "usage: itch-replay FILE [--max N] [--export SYM,SYM --out DIR] [--json OUT]\n");
        return 2;
    }
    std::string path = argv[1], out_dir = ".", json_path;
    uint64_t max_msgs = ~0ull;
    std::vector<std::string> exports;
    std::vector<uint64_t> trace;  // order references to print every message of
    bool by_ref = true;           // rank by reference number (entry order), not by arrival of the add
    for (int i = 2; i + 1 < argc; i += 2) {
        std::string a = argv[i], v = argv[i + 1];
        if (a == "--priority") by_ref = v != "arrival";
        else if (a == "--max") max_msgs = std::strtoull(v.c_str(), nullptr, 10);
        else if (a == "--out") out_dir = v;
        else if (a == "--json") json_path = v;
        else if (a == "--trace") {
            for (size_t s0 = 0; s0 < v.size();) {
                size_t e = v.find(',', s0);
                if (e == std::string::npos) e = v.size();
                trace.push_back(std::strtoull(v.substr(s0, e - s0).c_str(), nullptr, 10));
                s0 = e + 1;
            }
        }
        else if (a == "--export") {
            size_t s = 0;
            while (s <= v.size()) {
                size_t e = v.find(',', s);
                if (e == std::string::npos) e = v.size();
                if (e > s) exports.push_back(v.substr(s, e - s));
                s = e + 1;
            }
        }
    }
    std::FILE* f = std::fopen(path.c_str(), "rb");
    if (!f) {
        std::fprintf(stderr, "cannot open %s\n", path.c_str());
        return 1;
    }
    Reader rd(f);
    Engine eng(1 << 22);
    std::vector<Stock> stocks(1);
    eng.add_symbol("", 1);
    std::unordered_map<uint64_t, uint64_t> add_time;  // for exported stocks: when each order was added
    Stats st;
    Group g;
    std::vector<Fill> want;
    bool market_open = false;
    uint64_t next_sample = 0;
    const uint64_t SAMPLE = 100'000'000ull;  // 100 ms
    std::vector<uint16_t> exported;
    int mismatch_shown = 0;

    auto bbo = [&](uint16_t loc, int64_t& bid, uint64_t& bq, int64_t& ask, uint64_t& aq) {
        const SymbolBook& b = eng.book(loc);
        bid = b.bids.empty() ? 0 : b.bids.best().price;
        bq = b.bids.empty() ? 0 : b.bids.best().qty;
        ask = b.asks.empty() ? 0 : b.asks.best().price;
        aq = b.asks.empty() ? 0 : b.asks.best().qty;
    };
    auto check_cross = [&](uint16_t loc) {
        if (!market_open || stocks[loc].state != 'T') return;
        const SymbolBook& b = eng.book(loc);
        ++st.cross_checks;
        if (b.bids.empty() || b.asks.empty()) return;
        if (b.bids.best().price > b.asks.best().price) ++st.crossed;
        else if (b.bids.best().price == b.asks.best().price) ++st.locked;
    };
    auto log_event = [&](uint16_t loc, uint64_t ts, char type, uint8_t side, int64_t price, uint32_t shares, uint64_t age) {
        Stock& s = stocks[loc];
        if (!s.events) return;
        int64_t bid, ask;
        uint64_t bq, aq;
        bbo(loc, bid, bq, ask, aq);
        std::fprintf(s.events, "%llu,%c,%c,%lld,%u,%lld,%lld,%llu\n", (unsigned long long)ts, type, side_char(side), (long long)price, shares,
                     (long long)bid, (long long)ask, (unsigned long long)age);
    };
    auto flush = [&]() {
        if (!g.active) return;
        g.active = false;
        ++st.groups;
        Stock& s = stocks[g.loc];
        if (!market_open || s.state != 'T') ++st.g_outside;
        else if (g.has_c) ++st.g_with_c;
        else if (g.unknown) ++st.g_unknown;
        else {
            ++st.g_checked;
            eng.would_match(g.loc, g.rest_side ^ 1, g.worst, static_cast<uint32_t>(g.qty), want);
            bool ok = want.size() == g.fills.size();
            for (size_t i = 0; ok && i < want.size(); ++i) ok = want[i].resting == g.fills[i].resting && want[i].qty == g.fills[i].qty;
            st.fills_checked += g.fills.size();
            if (ok) ++st.g_exact;
            else {
                ++st.g_mismatch;
                if (mismatch_shown++ < 5) {
                    std::fprintf(stderr, "mismatch %s ts %llu: real", s.name.c_str(), (unsigned long long)g.ts);
                    for (auto& x : g.fills) std::fprintf(stderr, " %llu:%u", (unsigned long long)x.resting, x.qty);
                    std::fprintf(stderr, " | engine");
                    for (auto& x : want) std::fprintf(stderr, " %llu:%u", (unsigned long long)x.resting, x.qty);
                    std::fprintf(stderr, "\n");
                }
            }
        }
        for (auto& x : g.fills)
            if (!eng.reduce(x.resting, x.qty)) ++st.over_exec;
        g.fills.clear();
    };

    auto t0 = std::chrono::steady_clock::now();
    uint16_t len;
    const uint8_t* m;
    while (st.messages < max_msgs && (m = rd.next(len))) {
        uint8_t type = m[0];
        ++st.messages;
        ++st.by_type[type];
        uint16_t loc = locate(m);
        uint64_t ts = timestamp(m);
        if (len < length_of(type)) continue;  // malformed or unknown
        bool exec = type == 'E' || type == 'C';
        if (!trace.empty() && (type == 'A' || type == 'F' || exec || type == 'X' || type == 'D' || type == 'U')) {
            uint64_t r1 = be64(m + 11), r2 = type == 'U' ? be64(m + 19) : 0;
            for (uint64_t t : trace)
                if (t == r1 || t == r2) {
                    std::fprintf(stderr, "trace %c loc %u ts %llu ref %llu", type, loc, (unsigned long long)ts, (unsigned long long)r1);
                    if (type == 'A' || type == 'F') std::fprintf(stderr, " %c %u @ %u", m[19], be32(m + 20), be32(m + 32));
                    if (exec || type == 'X') std::fprintf(stderr, " shares %u", be32(m + 19));
                    if (type == 'C') std::fprintf(stderr, " printable %c @ %u", m[31], be32(m + 32));
                    if (type == 'U') std::fprintf(stderr, " -> %llu %u @ %u", (unsigned long long)r2, be32(m + 27), be32(m + 31));
                    std::fprintf(stderr, "\n");
                }
        }
        if (g.active && !(exec && loc == g.loc && ts == g.ts)) flush();

        if (market_open && ts >= next_sample && !exported.empty()) {
            for (; next_sample <= ts; next_sample += SAMPLE)
                for (uint16_t e : exported) {
                    int64_t bid, ask;
                    uint64_t bq, aq;
                    bbo(e, bid, bq, ask, aq);
                    std::fprintf(stocks[e].quotes, "%llu,%lld,%llu,%lld,%llu\n", (unsigned long long)next_sample, (long long)bid,
                                 (unsigned long long)bq, (long long)ask, (unsigned long long)aq);
                }
        }

        switch (type) {
            case 'S': {
                char code = static_cast<char>(m[11]);
                if (code == 'Q') {
                    market_open = true;
                    next_sample = ts - ts % SAMPLE + SAMPLE;
                } else if (code == 'M') market_open = false;
                break;
            }
            case 'R': {
                while (stocks.size() <= loc) {
                    stocks.emplace_back();
                    eng.add_symbol("", 1);
                }
                stocks[loc].name = trim(m + 11, 8);
                eng.book(loc).name = stocks[loc].name;
                for (const std::string& x : exports)
                    if (x == stocks[loc].name) {
                        std::string base = out_dir + "/" + x;
                        stocks[loc].events = std::fopen((base + "_events.csv").c_str(), "w");
                        stocks[loc].trades = std::fopen((base + "_trades.csv").c_str(), "w");
                        stocks[loc].quotes = std::fopen((base + "_quotes.csv").c_str(), "w");
                        if (!stocks[loc].events || !stocks[loc].trades || !stocks[loc].quotes) {
                            std::fprintf(stderr, "cannot write to %s\n", out_dir.c_str());
                            return 1;
                        }
                        std::fprintf(stocks[loc].events, "ts,type,side,price,shares,bid,ask,age_ns\n");
                        std::fprintf(stocks[loc].trades, "ts,price,shares,aggressor,kind\n");
                        std::fprintf(stocks[loc].quotes, "ts,bid,bid_size,ask,ask_size\n");
                        exported.push_back(loc);
                    }
                break;
            }
            case 'H':
                if (loc < stocks.size()) stocks[loc].state = static_cast<char>(m[19]);
                break;
            case 'A':
            case 'F': {
                uint64_t ref = be64(m + 11);
                uint8_t side = m[19] == 'B' ? BUY : SELL;
                uint32_t shares = be32(m + 20);
                int64_t price = be32(m + 32);
                if (loc >= stocks.size()) break;
                if (!eng.rest(loc, ref, side, price, shares, 0, 0, by_ref)) ++st.bad_replace;
                ++st.adds;
                if (stocks[loc].events) {
                    add_time[ref] = ts;
                    log_event(loc, ts, 'A', side, price, shares, 0);
                }
                check_cross(loc);
                break;
            }
            case 'E':
            case 'C': {
                uint64_t ref = be64(m + 11);
                uint32_t shares = be32(m + 19);
                const Order* o = eng.order(ref);
                if (!o) {
                    ++st.unknown_ref;
                    break;
                }
                int64_t px = o->price;
                bool printable = true;
                if (type == 'C') {
                    printable = m[31] == 'Y';
                    px = be32(m + 32);
                }
                if (!g.active) {
                    g = Group{true, loc, ts, o->side, false, false, o->price, 0, {}};
                } else if (o->side != g.rest_side) {
                    flush();
                    g = Group{true, loc, ts, o->side, false, false, o->price, 0, {}};
                }
                g.has_c |= type == 'C';
                if (g.rest_side == SELL ? o->price > g.worst : o->price < g.worst) g.worst = o->price;
                g.qty += shares;
                g.fills.push_back(Fill{ref, shares, o->price});
                st.exec_shares += shares;
                if (stocks[loc].trades && printable)
                    std::fprintf(stocks[loc].trades, "%llu,%lld,%u,%c,%c\n", (unsigned long long)ts, (long long)px, shares,
                                 side_char(o->side ^ 1), type);
                if (stocks[loc].events) {
                    auto it = add_time.find(ref);
                    log_event(loc, ts, static_cast<char>(type), o->side, px, shares, it == add_time.end() ? 0 : ts - it->second);
                }
                break;
            }
            case 'X': {
                uint64_t ref = be64(m + 11);
                uint32_t shares = be32(m + 19);
                const Order* o = eng.order(ref);
                if (!o) {
                    ++st.unknown_ref;
                    break;
                }
                if (stocks[loc].events) {
                    auto it = add_time.find(ref);
                    log_event(loc, ts, 'X', o->side, o->price, shares, it == add_time.end() ? 0 : ts - it->second);
                }
                if (!eng.reduce(ref, shares)) ++st.over_exec;
                break;
            }
            case 'D': {
                uint64_t ref = be64(m + 11);
                const Order* o = eng.order(ref);
                if (!o) {
                    ++st.unknown_ref;
                    break;
                }
                if (stocks[loc].events) {
                    auto it = add_time.find(ref);
                    log_event(loc, ts, 'D', o->side, o->price, o->qty, it == add_time.end() ? 0 : ts - it->second);
                    if (it != add_time.end()) add_time.erase(it);
                }
                eng.erase(ref);
                break;
            }
            case 'U': {
                uint64_t old_ref = be64(m + 11), new_ref = be64(m + 19);
                uint32_t shares = be32(m + 27);
                int64_t price = be32(m + 31);
                const Order* o = eng.order(old_ref);
                if (!o) {
                    ++st.unknown_ref;
                    break;
                }
                uint8_t side = o->side;
                uint32_t symbol = o->symbol;
                if (stocks[loc].events) {
                    auto it = add_time.find(old_ref);
                    log_event(loc, ts, 'U', side, price, shares, it == add_time.end() ? 0 : ts - it->second);
                    if (it != add_time.end()) add_time.erase(it);
                    add_time[new_ref] = ts;
                }
                eng.erase(old_ref);
                if (!eng.rest(symbol, new_ref, side, price, shares, 0, 0, by_ref)) ++st.bad_replace;
                check_cross(loc);
                break;
            }
            case 'P': {
                uint32_t shares = be32(m + 20);
                int64_t price = be32(m + 32);
                st.trade_shares += shares;
                if (loc < stocks.size() && stocks[loc].trades)
                    std::fprintf(stocks[loc].trades, "%llu,%lld,%u,%c,P\n", (unsigned long long)ts, (long long)price, shares, m[19] == 'B' ? 'S' : 'B');
                break;
            }
            case 'Q': {
                uint64_t shares = be64(m + 11);
                int64_t price = be32(m + 27);
                st.cross_shares += shares;
                if (loc < stocks.size() && stocks[loc].trades && shares)
                    std::fprintf(stocks[loc].trades, "%llu,%lld,%llu,X,Q\n", (unsigned long long)ts, (long long)price, (unsigned long long)shares);
                break;
            }
            default: break;
        }
        if ((st.messages & 0xFFFFF) == 0 && eng.live_orders() > st.max_live) st.max_live = eng.live_orders();
    }
    flush();
    double secs = std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    uint64_t live_end = eng.live_orders();
    for (Stock& s : stocks) {
        if (s.events) std::fclose(s.events);
        if (s.trades) std::fclose(s.trades);
        if (s.quotes) std::fclose(s.quotes);
    }

    std::string types;
    for (int t = 0; t < 256; ++t)
        if (st.by_type[t]) {
            char b[64];
            std::snprintf(b, sizeof b, "%s\"%c\": %llu", types.empty() ? "" : ", ", t, (unsigned long long)st.by_type[t]);
            types += b;
        }
    char json[4096];
    std::snprintf(json, sizeof json,
                  "{\n \"file\": \"%s\",\n \"priority\": \"%s\",\n \"messages\": %llu,\n \"bytes\": %llu,\n \"seconds\": %.3f,\n \"messages_per_second\": %.0f,\n"
                  " \"by_type\": {%s},\n \"stocks\": %zu,\n \"orders_added\": %llu,\n \"max_live_orders\": %llu,\n \"live_orders_at_end\": %llu,\n"
                  " \"unknown_order_references\": %llu,\n \"executions_or_cancels_over_open_quantity\": %llu,\n \"duplicate_adds\": %llu,\n"
                  " \"book_checks_in_regular_hours\": %llu,\n \"crossed\": %llu,\n \"locked\": %llu,\n"
                  " \"execution_groups\": %llu,\n \"groups_outside_regular_trading\": %llu,\n \"groups_with_price_executions\": %llu,\n"
                  " \"groups_checked\": %llu,\n \"groups_same_as_engine\": %llu,\n \"groups_different\": %llu,\n \"fills_checked\": %llu,\n"
                  " \"executed_shares\": %llu,\n \"hidden_trade_shares\": %llu,\n \"cross_shares\": %llu\n}\n",
                  path.c_str(), by_ref ? "reference" : "arrival", (unsigned long long)st.messages, (unsigned long long)rd.bytes(), secs, st.messages / secs, types.c_str(), stocks.size() - 1,
                  (unsigned long long)st.adds, (unsigned long long)st.max_live, (unsigned long long)live_end, (unsigned long long)st.unknown_ref,
                  (unsigned long long)st.over_exec, (unsigned long long)st.bad_replace, (unsigned long long)st.cross_checks,
                  (unsigned long long)st.crossed, (unsigned long long)st.locked, (unsigned long long)st.groups, (unsigned long long)st.g_outside,
                  (unsigned long long)st.g_with_c, (unsigned long long)st.g_checked, (unsigned long long)st.g_exact,
                  (unsigned long long)st.g_mismatch, (unsigned long long)st.fills_checked, (unsigned long long)st.exec_shares,
                  (unsigned long long)st.trade_shares, (unsigned long long)st.cross_shares);
    std::fputs(json, stdout);
    if (!json_path.empty()) {
        std::FILE* j = std::fopen(json_path.c_str(), "w");
        if (j) {
            std::fputs(json, j);
            std::fclose(j);
        }
    }
    return 0;
}
