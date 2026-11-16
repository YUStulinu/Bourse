// The matching engine: limit order books with price-time priority.
//
// One Engine is driven by one thread. Orders live in a pool (a vector with a
// free list) and are found by id through an open-addressing hash map. Each
// side of a book is a vector of price levels sorted from the worst price to
// the best, so the best level is at the back: most activity happens there,
// and adding or removing a level near the top moves few elements. Each level
// is a FIFO of orders linked by pool index.
#pragma once

#include <algorithm>
#include <cstdint>
#include <cstring>
#include <limits>
#include <string>
#include <vector>

#include "protocol.hpp"

namespace bourse {

constexpr uint32_t NIL = 0xFFFFFFFFu;
enum Side : uint8_t { BUY = 0, SELL = 1 };

inline uint8_t side_char(uint8_t s) { return s == BUY ? 'B' : 'S'; }

struct Order {
    uint64_t id;
    int64_t price;
    uint32_t qty;
    uint32_t account;
    uint32_t symbol;
    uint32_t next;
    uint32_t prev;
    uint16_t session;
    uint8_t side;
    uint8_t live;
};

struct Level {
    int64_t price;
    uint64_t qty;
    uint32_t head;
    uint32_t tail;
    uint32_t count;
};

/// Map from 64-bit order ids (never 0) to pool indices: linear probing,
/// deletion by backward shift (no tombstones), doubling at half full.
class IdMap {
public:
    explicit IdMap(size_t capacity = 1 << 16) { init(capacity); }

    uint32_t find(uint64_t key) const {
        size_t i = slot(key);
        while (true) {
            const Entry& e = t_[i];
            if (e.key == key) return e.value;
            if (e.key == 0) return NIL;
            i = (i + 1) & mask_;
        }
    }

    /// Insert; returns false when the key is already present.
    bool insert(uint64_t key, uint32_t value) {
        if ((size_ + 1) * 2 > t_.size()) grow();
        size_t i = slot(key);
        while (t_[i].key != 0) {
            if (t_[i].key == key) return false;
            i = (i + 1) & mask_;
        }
        t_[i] = {key, value};
        ++size_;
        return true;
    }

    void erase(uint64_t key) {
        size_t i = slot(key);
        while (t_[i].key != key) {
            if (t_[i].key == 0) return;
            i = (i + 1) & mask_;
        }
        // backward shift: move later entries of the cluster into the hole
        size_t j = i;
        while (true) {
            j = (j + 1) & mask_;
            if (t_[j].key == 0) break;
            size_t home = slot(t_[j].key);
            // the entry at j may move to i if i lies cyclically in [home, j)
            bool movable = (j > i) ? (home <= i || home > j) : (home <= i && home > j);
            if (movable) {
                t_[i] = t_[j];
                i = j;
            }
        }
        t_[i] = {0, 0};
        --size_;
    }

    size_t size() const { return size_; }

private:
    struct Entry {
        uint64_t key;
        uint32_t value;
    };
    std::vector<Entry> t_;
    size_t mask_ = 0, size_ = 0;
    int shift_ = 0;

    size_t slot(uint64_t key) const { return static_cast<size_t>((key * 0x9E3779B97F4A7C15ull) >> shift_); }

    void init(size_t capacity) {
        size_t c = 16;
        while (c < capacity) c <<= 1;
        t_.assign(c, Entry{0, 0});
        mask_ = c - 1;
        shift_ = 64 - __builtin_ctzll(c);
        size_ = 0;
    }

    void grow() {
        std::vector<Entry> old;
        old.swap(t_);
        init(old.size() * 2);
        for (const Entry& e : old)
            if (e.key) insert(e.key, e.value);
    }
};

/// One side of a book: levels from the worst price to the best.
class SideBook {
public:
    explicit SideBook(bool bid = true) : bid_(bid) {}

    bool bid() const { return bid_; }
    bool empty() const { return lv_.empty(); }
    size_t levels() const { return lv_.size(); }
    Level& best() { return lv_.back(); }
    const Level& best() const { return lv_.back(); }
    const Level& level_from_best(size_t k) const { return lv_[lv_.size() - 1 - k]; }
    void pop_best() { lv_.pop_back(); }

    /// true when price a is better than price b on this side
    bool better(int64_t a, int64_t b) const { return bid_ ? a > b : a < b; }

    /// index of the level at this price, or the insertion point with found=false
    size_t locate(int64_t price, bool& found) const {
        size_t n = lv_.size();
        // fast path: the best few levels, scanned from the back
        for (size_t k = 0; k < 8 && k < n; ++k) {
            size_t i = n - 1 - k;
            int64_t p = lv_[i].price;
            if (p == price) {
                found = true;
                return i;
            }
            if (better(price, p)) {
                found = false;
                return i + 1;
            }
        }
        if (n <= 8) {
            found = false;
            return 0;
        }
        // binary search over the rest, sorted from worst to best
        size_t lo = 0, hi = n - 8;
        while (lo < hi) {
            size_t mid = (lo + hi) / 2;
            if (better(price, lv_[mid].price))
                lo = mid + 1;
            else
                hi = mid;
        }
        found = lo < n && lv_[lo].price == price;
        return lo;
    }

    Level* find(int64_t price) {
        bool f;
        size_t i = locate(price, f);
        return f ? &lv_[i] : nullptr;
    }

    Level& find_or_insert(int64_t price) {
        bool f;
        size_t i = locate(price, f);
        if (!f) lv_.insert(lv_.begin() + static_cast<ptrdiff_t>(i), Level{price, 0, NIL, NIL, 0});
        return lv_[i];
    }

    void erase(int64_t price) {
        bool f;
        size_t i = locate(price, f);
        if (f) lv_.erase(lv_.begin() + static_cast<ptrdiff_t>(i));
    }

private:
    bool bid_;
    std::vector<Level> lv_;
};

struct SymbolBook {
    std::string name;
    uint32_t tick = 1;
    bool halted = false;
    SideBook bids{true};
    SideBook asks{false};
    SideBook& side(uint8_t s) { return s == BUY ? bids : asks; }
    const SideBook& side(uint8_t s) const { return s == BUY ? bids : asks; }
};

/// A fill the book would make, for checking priority without changing it.
struct Fill {
    uint64_t resting;
    uint32_t qty;
    int64_t price;
};

class Engine {
public:
    explicit Engine(size_t order_capacity = 1 << 16) : ids_(order_capacity) {
        pool_.reserve(order_capacity);
        blocked_.assign(1 << 20, 0);
    }

    uint32_t add_symbol(const std::string& name, uint32_t tick = 1) {
        books_.emplace_back();
        books_.back().name = name;
        books_.back().tick = tick ? tick : 1;
        return static_cast<uint32_t>(books_.size() - 1);
    }
    size_t symbols() const { return books_.size(); }
    SymbolBook& book(uint32_t s) { return books_[s]; }
    const SymbolBook& book(uint32_t s) const { return books_[s]; }
    size_t live_orders() const { return ids_.size(); }
    const Order* order(uint64_t id) const {
        uint32_t i = ids_.find(id);
        return i == NIL ? nullptr : &pool_[i];
    }

    // ---- the command path -------------------------------------------------

    template <class Sink>
    void process(const Command& c, Sink& sink, uint64_t now) {
        switch (c.type) {
            case CMD_NEW: on_new(c, c.order_id, sink, now, false); break;
            case CMD_CANCEL: on_cancel(c, sink, now); break;
            case CMD_REPLACE: on_replace(c, sink, now); break;
            case CMD_MASS_CANCEL: mass_cancel(c.account, c.symbol, CXL_MASS, sink, now); break;
            case CMD_BLOCK: {
                if (c.account < blocked_.size()) blocked_[c.account] = 1;
                mass_cancel(c.account, ALL_SYMBOLS, CXL_BLOCKED, sink, now);
                Event e{};
                e.type = EV_BLOCKED;
                e.account = c.account;
                e.session = c.session;
                e.ts = now;
                sink(e);
                break;
            }
            case CMD_UNBLOCK: {
                if (c.account < blocked_.size()) blocked_[c.account] = 0;
                Event e{};
                e.type = EV_UNBLOCKED;
                e.account = c.account;
                e.session = c.session;
                e.ts = now;
                sink(e);
                break;
            }
            default: break;
        }
    }

    // ---- direct book edits, for replaying a feed (no matching) -------------

    /// Put an order on the book at the back of its level, or, with by_id,
    /// behind every order of its level with a smaller id. NASDAQ assigns
    /// order reference numbers in the order orders are entered and ranks by
    /// them, so an order displayed late (one entered before the open for
    /// regular hours only) keeps the priority of its entry.
    bool rest(uint32_t symbol, uint64_t id, uint8_t side, int64_t price, uint32_t qty, uint32_t account = 0, uint16_t session = 0,
              bool by_id = false) {
        uint32_t oi = alloc();
        Order& o = pool_[oi];
        o = Order{id, price, qty, account, symbol, NIL, NIL, session, side, 1};
        if (!ids_.insert(id, oi)) {
            release(oi);
            return false;
        }
        Level& L = books_[symbol].side(side).find_or_insert(price);
        if (by_id && L.tail != NIL && pool_[L.tail].id > id)
            insert_sorted(L, oi);
        else
            append(L, oi);
        return true;
    }

    /// Take qty off an order (executed or cancelled); returns false when the
    /// order is unknown or has less open quantity.
    bool reduce(uint64_t id, uint32_t qty) {
        uint32_t oi = ids_.find(id);
        if (oi == NIL) return false;
        Order& o = pool_[oi];
        if (qty > o.qty) return false;
        SideBook& sb = books_[o.symbol].side(o.side);
        Level* L = sb.find(o.price);
        o.qty -= qty;
        L->qty -= qty;
        if (o.qty == 0) remove(oi, sb, *L);
        return true;
    }

    bool erase(uint64_t id) {
        uint32_t oi = ids_.find(id);
        if (oi == NIL) return false;
        Order& o = pool_[oi];
        SideBook& sb = books_[o.symbol].side(o.side);
        Level* L = sb.find(o.price);
        L->qty -= o.qty;
        remove(oi, sb, *L);
        return true;
    }

    /// The fills an incoming order would get, without changing the book.
    void would_match(uint32_t symbol, uint8_t side, int64_t limit, uint32_t qty, std::vector<Fill>& out) const {
        out.clear();
        const SideBook& opp = books_[symbol].side(side ^ 1);
        for (size_t k = 0; k < opp.levels() && qty; ++k) {
            const Level& L = opp.level_from_best(k);
            if (side == BUY ? L.price > limit : L.price < limit) break;
            for (uint32_t oi = L.head; oi != NIL && qty; oi = pool_[oi].next) {
                uint32_t x = std::min(qty, pool_[oi].qty);
                out.push_back(Fill{pool_[oi].id, x, L.price});
                qty -= x;
            }
        }
    }

    /// Sum of open quantity at prices as good as `limit` or better, up to `need`.
    uint64_t available(uint32_t symbol, uint8_t side, int64_t limit, uint64_t need) const {
        const SideBook& opp = books_[symbol].side(side ^ 1);
        uint64_t s = 0;
        for (size_t k = 0; k < opp.levels() && s < need; ++k) {
            const Level& L = opp.level_from_best(k);
            if (side == BUY ? L.price > limit : L.price < limit) break;
            s += L.qty;
        }
        return s;
    }

    /// Walk the orders of a level (front to back).
    template <class F>
    void for_each_order(const Level& L, F f) const {
        for (uint32_t oi = L.head; oi != NIL; oi = pool_[oi].next) f(pool_[oi]);
    }

private:
    std::vector<SymbolBook> books_;
    std::vector<Order> pool_;
    std::vector<uint32_t> free_;
    IdMap ids_;
    std::vector<uint8_t> blocked_;

    uint32_t alloc() {
        if (!free_.empty()) {
            uint32_t i = free_.back();
            free_.pop_back();
            return i;
        }
        pool_.emplace_back();
        return static_cast<uint32_t>(pool_.size() - 1);
    }
    void release(uint32_t oi) {
        pool_[oi].live = 0;
        free_.push_back(oi);
    }

    void append(Level& L, uint32_t oi) {
        Order& o = pool_[oi];
        o.prev = L.tail;
        o.next = NIL;
        if (L.tail != NIL)
            pool_[L.tail].next = oi;
        else
            L.head = oi;
        L.tail = oi;
        L.qty += o.qty;
        ++L.count;
    }

    /// Insert before the first order (from the back) with a smaller id.
    void insert_sorted(Level& L, uint32_t oi) {
        Order& o = pool_[oi];
        uint32_t after = L.tail;
        while (after != NIL && pool_[after].id > o.id) after = pool_[after].prev;
        o.prev = after;
        o.next = after == NIL ? L.head : pool_[after].next;
        if (after == NIL)
            L.head = oi;
        else
            pool_[after].next = oi;
        if (o.next == NIL)
            L.tail = oi;
        else
            pool_[o.next].prev = oi;
        L.qty += o.qty;
        ++L.count;
    }

    /// Unlink an order from its level (whose qty is already adjusted), free it,
    /// and drop the level when it becomes empty.
    void remove(uint32_t oi, SideBook& sb, Level& L) {
        Order& o = pool_[oi];
        if (o.prev != NIL)
            pool_[o.prev].next = o.next;
        else
            L.head = o.next;
        if (o.next != NIL)
            pool_[o.next].prev = o.prev;
        else
            L.tail = o.prev;
        --L.count;
        ids_.erase(o.id);
        int64_t price = o.price;
        release(oi);
        if (L.count == 0) {
            if (&L == &sb.best())
                sb.pop_best();
            else
                sb.erase(price);
        }
    }

    template <class Sink>
    void reject(const Command& c, uint64_t id, uint8_t reason, Sink& sink, uint64_t now) {
        Event e{};
        e.type = EV_REJECTED;
        e.side = c.side;
        e.reason = reason;
        e.session = c.session;
        e.symbol = c.symbol;
        e.account = c.account;
        e.order_id = id;
        e.order_id2 = c.type == CMD_REPLACE ? c.order_id : 0;
        e.price = c.price;
        e.qty = c.qty;
        e.ts = now;
        sink(e);
    }

    template <class Sink>
    void emit_cancel(const Order& o, uint32_t cancelled, uint8_t reason, Sink& sink, uint64_t now) {
        Event e{};
        e.type = EV_CANCELLED;
        e.side = side_char(o.side);
        e.reason = reason;
        e.session = o.session;
        e.symbol = o.symbol;
        e.account = o.account;
        e.order_id = o.id;
        e.price = o.price;
        e.qty = cancelled;
        e.qty2 = o.qty;
        e.ts = now;
        sink(e);
    }

    template <class Sink>
    void on_new(const Command& c, uint64_t id, Sink& sink, uint64_t now, bool replacing) {
        if (c.symbol >= books_.size()) return reject(c, id, REJ_SYMBOL, sink, now);
        SymbolBook& b = books_[c.symbol];
        uint8_t side = c.side == 'B' ? BUY : SELL;
        bool market = c.flags & F_MARKET;
        if (c.qty == 0 || c.qty > 1000000000u) return reject(c, id, REJ_QTY, sink, now);
        if (!market && (c.price <= 0 || c.price % b.tick != 0)) return reject(c, id, REJ_PRICE, sink, now);
        if (c.account < blocked_.size() && blocked_[c.account]) return reject(c, id, REJ_BLOCKED, sink, now);
        if (b.halted) return reject(c, id, REJ_HALTED, sink, now);
        if (id == 0 || ids_.find(id) != NIL) return reject(c, id, REJ_DUPLICATE, sink, now);
        int64_t limit = market ? (side == BUY ? std::numeric_limits<int64_t>::max() : std::numeric_limits<int64_t>::min()) : c.price;
        SideBook& opp = b.side(side ^ 1);
        bool crosses = !opp.empty() && (side == BUY ? opp.best().price <= limit : opp.best().price >= limit);
        if ((c.flags & F_POST_ONLY) && crosses) return reject(c, id, REJ_POST_ONLY, sink, now);

        if (!replacing) {
            Event a{};
            a.type = EV_ACCEPTED;
            a.side = c.side;
            a.session = c.session;
            a.symbol = c.symbol;
            a.account = c.account;
            a.order_id = id;
            a.price = c.price;
            a.qty = c.qty;
            a.flags = c.flags;
            a.reason = c.tif;
            a.ts = now;
            sink(a);
        }
        Order in{id, c.price, c.qty, c.account, c.symbol, NIL, NIL, c.session, side, 1};
        uint8_t tif = market ? static_cast<uint8_t>(TIF_IOC) : c.tif;
        if (tif == TIF_FOK && available(c.symbol, side, limit, c.qty) < c.qty) {
            in.qty = 0;
            return emit_cancel(in, c.qty, CXL_FOK, sink, now);
        }

        uint32_t left = crosses ? match(b, in, limit, c.flags & F_STP, sink, now) : c.qty;
        if (left == 0) return;
        in.qty = left;
        if (tif != TIF_DAY) {
            in.qty = 0;
            return emit_cancel(in, left, CXL_IOC, sink, now);
        }
        uint32_t oi = alloc();
        pool_[oi] = in;
        ids_.insert(id, oi);
        append(b.side(side).find_or_insert(c.price), oi);
        Event e{};
        e.type = EV_BOOKED;
        e.side = c.side;
        e.session = c.session;
        e.symbol = c.symbol;
        e.account = c.account;
        e.order_id = id;
        e.price = c.price;
        e.qty = left;
        e.ts = now;
        sink(e);
    }

    /// Match an incoming order against the opposite side; returns what is left.
    template <class Sink>
    uint32_t match(SymbolBook& b, const Order& in, int64_t limit, bool stp, Sink& sink, uint64_t now) {
        SideBook& opp = b.side(in.side ^ 1);
        uint32_t qty = in.qty;
        while (qty && !opp.empty()) {
            Level& L = opp.best();
            if (in.side == BUY ? L.price > limit : L.price < limit) break;
            int64_t px = L.price;
            uint32_t oi = L.head;
            while (qty && oi != NIL) {
                Order& o = pool_[oi];
                uint32_t next = o.next;
                bool last = next == NIL;
                if (stp && o.account == in.account) {
                    uint32_t q = o.qty;
                    o.qty = 0;
                    L.qty -= q;
                    emit_cancel(o, q, CXL_STP, sink, now);
                    remove(oi, opp, L);
                    if (last) break;  // the level may be gone
                    oi = next;
                    continue;
                }
                uint32_t x = std::min(qty, o.qty);
                o.qty -= x;
                L.qty -= x;
                qty -= x;
                Event e{};
                e.type = EV_EXECUTED;
                e.side = side_char(o.side);
                e.flags = o.qty == 0;
                e.session = o.session;
                e.session2 = in.session;
                e.symbol = in.symbol;
                e.account = o.account;
                e.order_id = o.id;
                e.order_id2 = in.id;
                e.price = px;
                e.qty = x;
                e.qty2 = qty;
                e.account2 = in.account;
                e.ts = now;
                sink(e);
                if (o.qty == 0) {
                    remove(oi, opp, L);
                    if (last) break;
                }
                oi = next;
            }
        }
        return qty;
    }

    template <class Sink>
    void on_cancel(const Command& c, Sink& sink, uint64_t now) {
        uint32_t oi = ids_.find(c.order_id);
        if (oi == NIL) return reject(c, c.order_id, REJ_UNKNOWN_ORDER, sink, now);
        Order& o = pool_[oi];
        uint32_t target = c.qty;
        if (target >= o.qty) return emit_cancel(o, 0, CXL_USER, sink, now);
        uint32_t cut = o.qty - target;
        SideBook& sb = books_[o.symbol].side(o.side);
        Level* L = sb.find(o.price);
        o.qty = target;
        L->qty -= cut;
        emit_cancel(o, cut, CXL_USER, sink, now);
        if (target == 0) remove(oi, sb, *L);
    }

    template <class Sink>
    void on_replace(const Command& c, Sink& sink, uint64_t now) {
        uint32_t oi = ids_.find(c.order_id);
        if (oi == NIL) return reject(c, c.new_order_id, REJ_UNKNOWN_ORDER, sink, now);
        Order& o = pool_[oi];
        if (c.new_order_id == 0 || ids_.find(c.new_order_id) != NIL) return reject(c, c.new_order_id, REJ_DUPLICATE, sink, now);
        if (c.qty == 0 || c.qty > 1000000000u) return reject(c, c.new_order_id, REJ_QTY, sink, now);
        SymbolBook& b = books_[o.symbol];
        if (c.price <= 0 || c.price % b.tick != 0) return reject(c, c.new_order_id, REJ_PRICE, sink, now);
        Command n = c;
        n.type = CMD_NEW;
        n.side = side_char(o.side);
        n.symbol = o.symbol;
        n.account = o.account;
        n.session = o.session;
        n.flags = c.flags & ~F_MARKET;
        n.tif = TIF_DAY;
        if ((n.flags & F_POST_ONLY)) {
            SideBook& opp = b.side(o.side ^ 1);
            if (!opp.empty() && (o.side == BUY ? opp.best().price <= c.price : opp.best().price >= c.price))
                return reject(c, c.new_order_id, REJ_POST_ONLY, sink, now);
        }
        Event e{};
        e.type = EV_REPLACED;
        e.side = n.side;
        e.session = o.session;
        e.symbol = o.symbol;
        e.account = o.account;
        e.order_id = o.id;
        e.order_id2 = c.new_order_id;
        e.price = c.price;
        e.qty = c.qty;
        e.qty2 = o.qty;
        e.ts = now;
        SideBook& sb = b.side(o.side);
        Level* L = sb.find(o.price);
        L->qty -= o.qty;
        remove(oi, sb, *L);
        sink(e);
        on_new(n, c.new_order_id, sink, now, true);
    }

    template <class Sink>
    void mass_cancel(uint32_t account, uint32_t symbol, uint8_t reason, Sink& sink, uint64_t now) {
        // rare: a scan of the pool is fine
        std::vector<uint64_t> ids;
        for (const Order& o : pool_)
            if (o.live && o.account == account && (symbol == ALL_SYMBOLS || o.symbol == symbol)) ids.push_back(o.id);
        for (uint64_t id : ids) {
            uint32_t oi = ids_.find(id);
            Order& o = pool_[oi];
            SideBook& sb = books_[o.symbol].side(o.side);
            Level* L = sb.find(o.price);
            uint32_t q = o.qty;
            L->qty -= q;
            o.qty = 0;
            emit_cancel(o, q, reason, sink, now);
            remove(oi, sb, *L);
        }
    }
};

}  // namespace bourse
