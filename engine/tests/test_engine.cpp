// Tests of the matching engine, the id map and the bus.
#include <atomic>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <map>
#include <random>
#include <thread>
#include <unordered_map>
#include <vector>

#include "bourse/bus.hpp"
#include "bourse/engine.hpp"

using namespace bourse;

static int g_failed = 0, g_checks = 0;
#define CHECK(cond)                                                                 \
    do {                                                                            \
        ++g_checks;                                                                 \
        if (!(cond)) {                                                              \
            ++g_failed;                                                             \
            std::fprintf(stderr, "%s:%d: CHECK failed: %s\n", __FILE__, __LINE__, #cond); \
        }                                                                           \
    } while (0)

struct Rec {
    std::vector<Event> ev;
    void operator()(const Event& e) { ev.push_back(e); }
};

static Command order(uint64_t id, char side, int64_t price, uint32_t qty, uint32_t account = 1, uint8_t tif = TIF_DAY, uint8_t flags = 0) {
    Command c{};
    c.type = CMD_NEW;
    c.side = static_cast<uint8_t>(side);
    c.tif = tif;
    c.flags = flags;
    c.symbol = 0;
    c.account = account;
    c.order_id = id;
    c.price = price;
    c.qty = qty;
    return c;
}

static Command cancel(uint64_t id, uint32_t leave = 0) {
    Command c{};
    c.type = CMD_CANCEL;
    c.order_id = id;
    c.qty = leave;
    return c;
}

static Command replace(uint64_t id, uint64_t nid, int64_t price, uint32_t qty) {
    Command c{};
    c.type = CMD_REPLACE;
    c.order_id = id;
    c.new_order_id = nid;
    c.price = price;
    c.qty = qty;
    return c;
}

static std::vector<std::pair<uint64_t, uint32_t>> fills(const Rec& r) {
    std::vector<std::pair<uint64_t, uint32_t>> f;
    for (auto& e : r.ev)
        if (e.type == EV_EXECUTED) f.push_back({e.order_id, e.qty});
    return f;
}

static void test_priority() {
    Engine e;
    e.add_symbol("TEST", 1);
    Rec r;
    e.process(order(1, 'S', 100, 10), r, 0);
    e.process(order(2, 'S', 100, 10), r, 0);
    e.process(order(3, 'S', 99, 5), r, 0);
    e.process(order(4, 'S', 101, 5), r, 0);
    r.ev.clear();
    e.process(order(5, 'B', 100, 18, 2), r, 0);
    auto f = fills(r);
    CHECK(f.size() == 3);
    CHECK(f[0] == std::make_pair(uint64_t(3), 5u));   // best price first
    CHECK(f[1] == std::make_pair(uint64_t(1), 10u));  // then time
    CHECK(f[2] == std::make_pair(uint64_t(2), 3u));
    CHECK(r.ev.front().type == EV_ACCEPTED);
    CHECK(r.ev.back().type == EV_EXECUTED);  // fully filled: nothing booked
    CHECK(e.order(2)->qty == 7);
    CHECK(e.book(0).asks.best().price == 100 && e.book(0).asks.best().qty == 7);
    CHECK(r.ev[1].price == 99 && r.ev[2].price == 100);  // at the resting price
    CHECK(r.ev[1].account == 1 && r.ev[1].account2 == 2 && r.ev[1].order_id2 == 5);
}

static void test_tif_and_flags() {
    Engine e;
    e.add_symbol("TEST", 5);
    Rec r;
    e.process(order(1, 'S', 100, 10), r, 0);
    // tick size
    r.ev.clear();
    e.process(order(2, 'B', 102, 1), r, 0);
    CHECK(r.ev.size() == 1 && r.ev[0].type == EV_REJECTED && r.ev[0].reason == REJ_PRICE);
    // immediate-or-cancel: fills 10, cancels 5
    r.ev.clear();
    e.process(order(3, 'B', 100, 15, 2, TIF_IOC), r, 0);
    CHECK(r.ev.size() == 3 && r.ev[1].type == EV_EXECUTED && r.ev[2].type == EV_CANCELLED && r.ev[2].reason == CXL_IOC && r.ev[2].qty == 5);
    CHECK(e.book(0).asks.empty() && e.book(0).bids.empty());
    // fill-or-kill: not enough
    e.process(order(4, 'S', 100, 10), r, 0);
    r.ev.clear();
    e.process(order(5, 'B', 105, 11, 2, TIF_FOK), r, 0);
    CHECK(r.ev.size() == 2 && r.ev[1].type == EV_CANCELLED && r.ev[1].reason == CXL_FOK && r.ev[1].qty == 11);
    CHECK(e.order(4)->qty == 10);
    r.ev.clear();
    e.process(order(6, 'B', 105, 10, 2, TIF_FOK), r, 0);
    CHECK(fills(r).size() == 1 && e.book(0).asks.empty());
    // post only
    e.process(order(7, 'S', 110, 10), r, 0);
    r.ev.clear();
    e.process(order(8, 'B', 110, 1, 2, TIF_DAY, F_POST_ONLY), r, 0);
    CHECK(r.ev.size() == 1 && r.ev[0].reason == REJ_POST_ONLY);
    r.ev.clear();
    e.process(order(9, 'B', 105, 1, 2, TIF_DAY, F_POST_ONLY), r, 0);
    CHECK(r.ev.size() == 2 && r.ev[1].type == EV_BOOKED);
    // market order sweeps
    e.process(order(10, 'S', 120, 10), r, 0);
    r.ev.clear();
    e.process(order(11, 'B', 0, 15, 2, TIF_DAY, F_MARKET), r, 0);
    CHECK(fills(r).size() == 2 && r.ev.back().type == EV_EXECUTED && e.order(10)->qty == 5);
    // self-trade prevention cancels the resting order of the same account
    e.process(order(12, 'S', 120, 3, 7), r, 0);
    r.ev.clear();
    e.process(order(13, 'B', 120, 6, 7, TIF_DAY, F_STP), r, 0);
    CHECK(r.ev.size() == 4);  // accepted, executed against 10 (account 1), STP-cancelled 12, booked 1
    CHECK(r.ev[1].type == EV_EXECUTED && r.ev[1].order_id == 10 && r.ev[1].qty == 5);
    CHECK(r.ev[2].type == EV_CANCELLED && r.ev[2].reason == CXL_STP && r.ev[2].order_id == 12);
    CHECK(r.ev[3].type == EV_BOOKED && r.ev[3].qty == 1);
    // duplicate id
    r.ev.clear();
    e.process(order(13, 'B', 100, 1), r, 0);
    CHECK(r.ev.size() == 1 && r.ev[0].reason == REJ_DUPLICATE);
}

static void test_cancel_replace() {
    Engine e;
    e.add_symbol("TEST", 1);
    Rec r;
    e.process(order(1, 'B', 100, 10), r, 0);
    e.process(order(2, 'B', 100, 10), r, 0);
    // reducing keeps priority
    r.ev.clear();
    e.process(cancel(1, 4), r, 0);
    CHECK(r.ev.size() == 1 && r.ev[0].type == EV_CANCELLED && r.ev[0].qty == 6 && r.ev[0].qty2 == 4);
    r.ev.clear();
    e.process(order(3, 'S', 100, 5, 2), r, 0);
    auto f = fills(r);
    CHECK(f.size() == 2 && f[0].first == 1 && f[0].second == 4 && f[1].first == 2 && f[1].second == 1);
    // replace loses priority, keeps price
    e.process(order(4, 'B', 100, 10), r, 0);
    e.process(replace(2, 20, 100, 9), r, 0);
    r.ev.clear();
    e.process(order(5, 'S', 100, 5, 2), r, 0);
    f = fills(r);
    CHECK(f.size() == 1 && f[0].first == 4);
    // a replace can cross
    e.process(order(6, 'S', 101, 3, 2), r, 0);
    r.ev.clear();
    e.process(replace(20, 21, 101, 9), r, 0);
    CHECK(r.ev[0].type == EV_REPLACED && r.ev[0].order_id == 20 && r.ev[0].order_id2 == 21);
    CHECK(fills(r).size() == 1 && r.ev.back().type == EV_BOOKED && r.ev.back().qty == 6);
    CHECK(e.order(20) == nullptr && e.order(21)->qty == 6);
    // unknown order
    r.ev.clear();
    e.process(cancel(999), r, 0);
    CHECK(r.ev.size() == 1 && r.ev[0].reason == REJ_UNKNOWN_ORDER);
    // full cancel removes the level
    e.process(cancel(21), r, 0);
    e.process(cancel(4), r, 0);
    CHECK(e.book(0).bids.levels() == 0 || e.book(0).bids.best().price != 101);
    // mass cancel and blocking
    e.process(order(30, 'B', 90, 1, 9), r, 0);
    e.process(order(31, 'S', 190, 1, 9), r, 0);
    Command blk{};
    blk.type = CMD_BLOCK;
    blk.account = 9;
    r.ev.clear();
    e.process(blk, r, 0);
    CHECK(r.ev.size() == 3 && r.ev[0].reason == CXL_BLOCKED && r.ev[2].type == EV_BLOCKED);
    r.ev.clear();
    e.process(order(32, 'B', 90, 1, 9), r, 0);
    CHECK(r.ev.size() == 1 && r.ev[0].reason == REJ_BLOCKED);
    blk.type = CMD_UNBLOCK;
    e.process(blk, r, 0);
    r.ev.clear();
    e.process(order(33, 'B', 90, 1, 9), r, 0);
    CHECK(r.ev.back().type == EV_BOOKED);
}

static void test_idmap() {
    IdMap m(16);
    std::unordered_map<uint64_t, uint32_t> ref;
    std::mt19937_64 rng(1);
    for (int i = 0; i < 400000; ++i) {
        uint64_t k = 1 + rng() % 5000;
        int op = static_cast<int>(rng() % 3);
        if (op == 0) {
            bool fresh = ref.find(k) == ref.end();
            CHECK(m.insert(k, static_cast<uint32_t>(i)) == fresh);
            if (fresh) ref[k] = static_cast<uint32_t>(i);
        } else if (op == 1) {
            m.erase(k);
            ref.erase(k);
        } else {
            auto it = ref.find(k);
            CHECK(m.find(k) == (it == ref.end() ? NIL : it->second));
        }
    }
    CHECK(m.size() == ref.size());
}

// ---- a naive reference engine: lists of orders, best found by scanning ------

struct RefEngine {
    struct O {
        uint64_t id, seq;
        int64_t price;
        uint32_t qty, account;
        uint8_t side;
    };
    std::vector<O> orders;
    uint64_t seq = 0;
    uint32_t tick = 1;

    O* find(uint64_t id) {
        for (auto& o : orders)
            if (o.id == id) return &o;
        return nullptr;
    }
    void erase(uint64_t id) {
        for (size_t i = 0; i < orders.size(); ++i)
            if (orders[i].id == id) {
                orders.erase(orders.begin() + static_cast<ptrdiff_t>(i));
                return;
            }
    }
    // the best resting order against an incoming order of this side
    O* best(uint8_t side) {
        O* b = nullptr;
        for (auto& o : orders) {
            if (o.side == side) continue;
            if (!b || (side == BUY ? o.price < b->price : o.price > b->price) || (o.price == b->price && o.seq < b->seq)) b = &o;
        }
        return b;
    }
    Event ev(uint8_t type, const O& o) {
        Event e{};
        e.type = type;
        e.side = side_char(o.side);
        e.account = o.account;
        e.order_id = o.id;
        e.price = o.price;
        return e;
    }
    void incoming(O in, uint8_t tif, uint8_t flags, std::vector<Event>& out) {
        int64_t limit = in.price;
        if (tif == TIF_FOK) {
            uint64_t s = 0;
            for (auto& o : orders)
                if (o.side != in.side && (in.side == BUY ? o.price <= limit : o.price >= limit)) s += o.qty;
            if (s < in.qty) {
                Event c = ev(EV_CANCELLED, in);
                c.reason = CXL_FOK;
                c.qty = in.qty;
                out.push_back(c);
                return;
            }
        }
        while (in.qty) {
            O* b = best(in.side);
            if (!b || (in.side == BUY ? b->price > limit : b->price < limit)) break;
            if ((flags & F_STP) && b->account == in.account) {
                Event c = ev(EV_CANCELLED, *b);
                c.reason = CXL_STP;
                c.qty = b->qty;
                out.push_back(c);
                erase(b->id);
                continue;
            }
            uint32_t x = std::min(in.qty, b->qty);
            b->qty -= x;
            in.qty -= x;
            Event e = ev(EV_EXECUTED, *b);
            e.qty = x;
            e.qty2 = in.qty;
            e.order_id2 = in.id;
            e.account2 = in.account;
            e.flags = b->qty == 0;
            out.push_back(e);
            if (b->qty == 0) erase(b->id);
        }
        if (!in.qty) return;
        if (tif != TIF_DAY) {
            Event c = ev(EV_CANCELLED, in);
            c.reason = CXL_IOC;
            c.qty = in.qty;
            out.push_back(c);
            return;
        }
        in.seq = seq++;
        orders.push_back(in);
        Event b = ev(EV_BOOKED, in);
        b.qty = in.qty;
        out.push_back(b);
    }
    void process(const Command& c, std::vector<Event>& out) {
        if (c.type == CMD_NEW) {
            if (find(c.order_id)) {
                Event r{};
                r.type = EV_REJECTED;
                r.reason = REJ_DUPLICATE;
                r.order_id = c.order_id;
                out.push_back(r);
                return;
            }
            O in{c.order_id, 0, c.price, c.qty, c.account, static_cast<uint8_t>(c.side == 'B' ? BUY : SELL)};
            Event a = ev(EV_ACCEPTED, in);
            a.qty = c.qty;
            out.push_back(a);
            incoming(in, c.tif, c.flags, out);
        } else if (c.type == CMD_CANCEL) {
            O* o = find(c.order_id);
            if (!o) {
                Event r{};
                r.type = EV_REJECTED;
                r.reason = REJ_UNKNOWN_ORDER;
                r.order_id = c.order_id;
                out.push_back(r);
                return;
            }
            uint32_t cut = c.qty >= o->qty ? 0 : o->qty - c.qty;
            o->qty -= cut;
            Event e = ev(EV_CANCELLED, *o);
            e.reason = CXL_USER;
            e.qty = cut;
            e.qty2 = o->qty;
            out.push_back(e);
            if (o->qty == 0) erase(o->id);
        } else if (c.type == CMD_REPLACE) {
            O* o = find(c.order_id);
            if (!o || find(c.new_order_id)) {
                Event r{};
                r.type = EV_REJECTED;
                r.reason = o ? REJ_DUPLICATE : REJ_UNKNOWN_ORDER;
                r.order_id = c.new_order_id;
                out.push_back(r);
                return;
            }
            O old = *o;
            erase(o->id);
            Event e = ev(EV_REPLACED, old);
            e.order_id2 = c.new_order_id;
            e.price = c.price;
            e.qty = c.qty;
            e.qty2 = old.qty;
            out.push_back(e);
            O in{c.new_order_id, 0, c.price, c.qty, old.account, old.side};
            incoming(in, TIF_DAY, 0, out);
        }
    }
};

// compare the fields the reference engine fills in
static bool same(const Event& a, const Event& b) {
    if (a.type != b.type || a.order_id != b.order_id) return false;
    switch (a.type) {
        case EV_REJECTED: return a.reason == b.reason;
        case EV_EXECUTED:
            return a.qty == b.qty && a.qty2 == b.qty2 && a.price == b.price && a.order_id2 == b.order_id2 && a.account == b.account &&
                   a.account2 == b.account2 && a.flags == b.flags && a.side == b.side;
        case EV_CANCELLED: return a.qty == b.qty && a.reason == b.reason && (a.reason != CXL_USER || a.qty2 == b.qty2);
        case EV_BOOKED:
        case EV_ACCEPTED: return a.qty == b.qty && a.price == b.price && a.side == b.side;
        case EV_REPLACED: return a.order_id2 == b.order_id2 && a.qty == b.qty && a.qty2 == b.qty2 && a.price == b.price;
        default: return true;
    }
}

static void test_against_reference() {
    for (uint64_t seed = 1; seed <= 20; ++seed) {
        std::mt19937_64 rng(seed);
        Engine e;
        e.add_symbol("TEST", 1);
        RefEngine ref;
        std::vector<uint64_t> live;
        uint64_t next_id = 1;
        size_t n_events = 0, mismatches = 0;
        for (int step = 0; step < 20000; ++step) {
            Command c{};
            int op = static_cast<int>(rng() % 10);
            if (op < 6 || live.empty()) {
                c = order(next_id++, rng() % 2 ? 'B' : 'S', 95 + static_cast<int64_t>(rng() % 11), 1 + static_cast<uint32_t>(rng() % 20),
                          1 + static_cast<uint32_t>(rng() % 4));
                int t = static_cast<int>(rng() % 10);
                c.tif = t < 7 ? TIF_DAY : (t < 9 ? TIF_IOC : TIF_FOK);
                if (rng() % 8 == 0) c.flags = F_STP;
                if (rng() % 50 == 0) c.order_id = live.empty() ? c.order_id : live[rng() % live.size()];  // duplicate
                live.push_back(c.order_id);
            } else if (op < 9) {
                uint64_t id = live[rng() % live.size()];
                c = cancel(id, rng() % 2 ? 0 : static_cast<uint32_t>(rng() % 10));
            } else {
                uint64_t id = live[rng() % live.size()];
                c = replace(id, next_id++, 95 + static_cast<int64_t>(rng() % 11), 1 + static_cast<uint32_t>(rng() % 20));
                live.push_back(c.new_order_id);
            }
            Rec r;
            std::vector<Event> want;
            e.process(c, r, 0);
            ref.process(c, want);
            n_events += want.size();
            if (r.ev.size() != want.size()) {
                ++mismatches;
                continue;
            }
            for (size_t i = 0; i < want.size(); ++i)
                if (!same(r.ev[i], want[i])) {
                    ++mismatches;
                    break;
                }
        }
        CHECK(mismatches == 0);
        CHECK(e.live_orders() == ref.orders.size());
        if (seed == 1) std::printf("  reference comparison: %zu events in seed 1\n", n_events);
    }
}

static void test_bus() {
    namespace fs = std::filesystem;
    fs::path p = fs::temp_directory_path() / "bourse_test_bus.dat";
    BusConfig cfg;
    cfg.n_in = 4;
    cfg.in_cap = 256;  // small, so producers and consumers wrap many times
    cfg.out_cap = 1024;
    Bus bus;
    CHECK(bus.create(p.string(), cfg));
    Bus view;
    CHECK(view.open(p.string()));
    CHECK(view.n_in() == 4 && view.in_cap() == 256 && view.out_cap() == 1024);
    const uint64_t N = 200000;
    // two producers on their own rings
    RingProducer p0, p1;
    CHECK(p0.attach(view));
    CHECK(p1.attach(view));
    CHECK(p0.index() != p1.index());
    // two consumers of the log
    LogConsumer c0, c1;
    CHECK(c0.attach(view));
    CHECK(c1.attach(view));
    std::atomic<bool> done{false};
    std::thread engine([&] {
        LogProducer log(bus);
        std::vector<uint64_t> tail(cfg.n_in, 0);
        uint64_t seen = 0;
        while (seen < 2 * N) {
            for (uint32_t r = 0; r < cfg.n_in; ++r) {
                uint64_t h = bus.ring_head(r).load(std::memory_order_acquire);
                for (; tail[r] < h; ++tail[r], ++seen) {
                    const Command* c = bus.ring_slot(r, tail[r]);
                    Event e{};
                    e.type = EV_ACCEPTED;
                    e.order_id = c->order_id;
                    e.session = static_cast<uint16_t>(r);
                    log.write(e);
                    log.publish();
                }
                bus.ring_tail(r).store(tail[r], std::memory_order_release);
            }
        }
        done = true;
    });
    auto produce = [&](RingProducer& pr) {
        for (uint64_t i = 1; i <= N; ++i) {
            Command c{};
            c.type = CMD_NEW;
            c.order_id = i;
            pr.push(c);
        }
    };
    std::thread t0(produce, std::ref(p0)), t1(produce, std::ref(p1));
    auto consume = [&](LogConsumer& lc, std::vector<uint64_t>& last, uint64_t& total, bool& ordered) {
        Event buf[64];
        while (total < 2 * N) {
            size_t n = lc.poll(buf, 64);
            for (size_t i = 0; i < n; ++i) {
                uint64_t& l = last[buf[i].session];
                if (buf[i].order_id != l + 1) ordered = false;
                l = buf[i].order_id;
            }
            total += n;
            lc.heartbeat();
        }
    };
    std::vector<uint64_t> l0(cfg.n_in, 0), l1(cfg.n_in, 0);
    uint64_t n0 = 0, n1 = 0;
    bool o0 = true, o1 = true;
    std::thread r0(consume, std::ref(c0), std::ref(l0), std::ref(n0), std::ref(o0));
    std::thread r1(consume, std::ref(c1), std::ref(l1), std::ref(n1), std::ref(o1));
    t0.join();
    t1.join();
    engine.join();
    r0.join();
    r1.join();
    CHECK(done && n0 == 2 * N && n1 == 2 * N && o0 && o1);
    CHECK(l0[p0.index()] == N && l0[p1.index()] == N);
}

int main() {
    test_priority();
    test_tif_and_flags();
    test_cancel_replace();
    test_idmap();
    test_against_reference();
    test_bus();
    std::printf("%d checks, %d failed\n", g_checks, g_failed);
    return g_failed ? 1 : 0;
}
