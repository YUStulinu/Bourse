// A C interface to the matching engine, for the Python market simulation
// (ctypes): the same Engine as the exchange, called in process.
#include <vector>

#include "bourse/engine.hpp"

#ifdef _WIN32
#define API extern "C" __declspec(dllexport)
#else
#define API extern "C" __attribute__((visibility("default")))
#endif

using namespace bourse;

namespace {
struct Handle {
    Engine eng{1 << 16};
    std::vector<Event> events;
};
struct VecSink {
    std::vector<Event>& v;
    void operator()(const Event& e) { v.push_back(e); }
};
}  // namespace

API void* bourse_new() { return new Handle(); }
API void bourse_free(void* h) { delete static_cast<Handle*>(h); }

API uint32_t bourse_add_symbol(void* h, const char* name, uint32_t tick) { return static_cast<Handle*>(h)->eng.add_symbol(name, tick); }

/// Process one command; returns the number of events, readable with bourse_events.
API int bourse_submit(void* h, const Command* c, uint64_t now) {
    auto* p = static_cast<Handle*>(h);
    p->events.clear();
    VecSink s{p->events};
    p->eng.process(*c, s, now);
    return static_cast<int>(p->events.size());
}

/// Process n commands in a row; events of all of them are kept.
API int bourse_submit_many(void* h, const Command* c, int n, uint64_t now) {
    auto* p = static_cast<Handle*>(h);
    p->events.clear();
    VecSink s{p->events};
    for (int i = 0; i < n; ++i) p->eng.process(c[i], s, now);
    return static_cast<int>(p->events.size());
}

API const Event* bourse_events(void* h) { return static_cast<Handle*>(h)->events.data(); }

/// Best `levels` price levels of one side (0 bids, 1 asks); returns how many.
API int bourse_depth(void* h, uint32_t symbol, int side, int levels, int64_t* prices, uint64_t* qtys) {
    const SideBook& sb = static_cast<Handle*>(h)->eng.book(symbol).side(static_cast<uint8_t>(side));
    int n = 0;
    for (size_t k = 0; k < sb.levels() && n < levels; ++k, ++n) {
        const Level& L = sb.level_from_best(k);
        prices[n] = L.price;
        qtys[n] = L.qty;
    }
    return n;
}

API uint64_t bourse_live_orders(void* h) { return static_cast<Handle*>(h)->eng.live_orders(); }
