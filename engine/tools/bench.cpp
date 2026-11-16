// engine-bench: latency and throughput of the matching engine.
//
//   engine-bench [--commands N] [--json OUT] [--cpus E,P,C]
//
// The workload is a synthetic order flow on 8 symbols, generated once by
// running it on a scratch engine (so cancels and replaces name live orders):
// passive limit orders a geometric number of ticks from a random-walk mid,
// cancels, replaces, and marketable orders (immediate-or-cancel and day).
//
//  1. in process: the time of each Engine::process call (read with the time
//     stamp counter), and the throughput without per-call timing;
//  2. over the bus, three threads as in the exchange: a producer writes
//     commands into an inbound ring on a fixed schedule (open loop), the
//     engine loop matches them, and a reader of the event log records when
//     the first event of each command appears. Latency is counted from the
//     scheduled send time, so a producer that falls behind is measured too
//     (no coordinated omission).
#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <filesystem>
#include <random>
#include <string>
#include <thread>
#include <unordered_map>
#include <vector>

#include "bourse/bus.hpp"
#include "bourse/engine.hpp"
#include "bourse/loop.hpp"

#if defined(_MSC_VER) && !defined(__clang__)
#include <intrin.h>
#else
#include <x86intrin.h>
#endif

using namespace bourse;

namespace {

constexpr int SYMBOLS = 8;
constexpr int64_t TICK = 100;

double g_tsc_per_ns = 1.0;

void calibrate() {
    auto t0 = std::chrono::steady_clock::now();
    uint64_t c0 = __rdtsc();
    while (std::chrono::steady_clock::now() - t0 < std::chrono::milliseconds(300)) {
    }
    uint64_t c1 = __rdtsc();
    double ns = std::chrono::duration<double, std::nano>(std::chrono::steady_clock::now() - t0).count();
    g_tsc_per_ns = (c1 - c0) / ns;
}

struct Null {
    void operator()(const Event&) {}
};

std::vector<Command> workload(size_t n, uint64_t seed) {
    std::mt19937_64 rng(seed);
    std::uniform_real_distribution<double> U(0, 1);
    std::geometric_distribution<int> dist(0.3), lots(0.5);
    Engine e(1 << 20);
    for (int s = 0; s < SYMBOLS; ++s) e.add_symbol("S" + std::to_string(s), TICK);
    std::vector<int64_t> mid(SYMBOLS, 1'000'000);
    std::vector<uint64_t> live;
    std::unordered_map<uint64_t, size_t> pos;
    auto add_live = [&](uint64_t id) {
        pos[id] = live.size();
        live.push_back(id);
    };
    auto drop_live = [&](uint64_t id) {
        auto it = pos.find(id);
        if (it == pos.end()) return;
        size_t i = it->second;
        pos[live.back()] = i;
        live[i] = live.back();
        live.pop_back();
        pos.erase(id);
    };
    struct Track {
        decltype(add_live)& add;
        decltype(drop_live)& drop;
        void operator()(const Event& ev) {
            if (ev.type == EV_BOOKED) add(ev.order_id);
            else if (ev.type == EV_EXECUTED && (ev.flags & 1)) drop(ev.order_id);
            else if (ev.type == EV_CANCELLED && ev.qty2 == 0) drop(ev.order_id);
            else if (ev.type == EV_REPLACED) drop(ev.order_id);
        }
    } track{add_live, drop_live};
    std::vector<Command> out;
    out.reserve(n);
    uint64_t next_id = 1;
    for (size_t i = 0; i < n; ++i) {
        uint32_t s = static_cast<uint32_t>(rng() % SYMBOLS);
        if (U(rng) < 0.02) mid[s] += U(rng) < 0.5 ? TICK : -TICK;
        double r = U(rng);
        Command c{};
        c.symbol = s;
        c.account = 1 + static_cast<uint32_t>(rng() % 64);
        c.session = 1;
        bool buy = U(rng) < 0.5;
        c.side = buy ? 'B' : 'S';
        if ((r < 0.80 && r >= 0.50 && !live.empty()) || (r >= 0.80 && r < 0.85 && !live.empty())) {
            uint64_t id = live[rng() % live.size()];
            const Order* o = e.order(id);
            if (r < 0.80) {
                c.type = CMD_CANCEL;
                c.order_id = id;
                c.qty = 0;
            } else {
                c.type = CMD_REPLACE;
                c.order_id = id;
                c.new_order_id = next_id++;
                int64_t m = mid[o->symbol];
                c.price = o->side == BUY ? m - (1 + dist(rng)) * TICK : m + (1 + dist(rng)) * TICK;
                c.qty = 100 * (1 + lots(rng));
            }
        } else {
            c.type = CMD_NEW;
            c.order_id = next_id++;
            c.qty = 100 * (1 + lots(rng));
            if (r < 0.85 || live.empty()) {
                int64_t d = (1 + dist(rng)) * TICK;
                c.price = buy ? mid[s] - d : mid[s] + d;
            } else {
                int64_t d = static_cast<int64_t>(rng() % 3) * TICK;
                c.price = buy ? mid[s] + d : mid[s] - d;
                c.tif = r < 0.95 ? TIF_IOC : TIF_DAY;
            }
        }
        e.process(c, track, 0);
        out.push_back(c);
    }
    return out;
}

struct Pct {
    double p50, p90, p99, p999, p9999, max, mean;
};

Pct percentiles(std::vector<uint64_t>& v) {
    std::sort(v.begin(), v.end());
    auto at = [&](double q) { return v[std::min(v.size() - 1, static_cast<size_t>(q * v.size()))] / g_tsc_per_ns; };
    double sum = 0;
    for (uint64_t x : v) sum += x;
    return Pct{at(0.5), at(0.9), at(0.99), at(0.999), at(0.9999), v.back() / g_tsc_per_ns, sum / v.size() / g_tsc_per_ns};
}

std::string pct_json(const Pct& p) {
    char b[256];
    std::snprintf(b, sizeof b, "{\"p50\": %.0f, \"p90\": %.0f, \"p99\": %.0f, \"p99_9\": %.0f, \"p99_99\": %.0f, \"max\": %.0f, \"mean\": %.1f}", p.p50,
                  p.p90, p.p99, p.p999, p.p9999, p.max, p.mean);
    return b;
}

struct BusResult {
    double rate;  // commands per second asked for (0: as fast as possible)
    double achieved;
    Pct lat;
};

BusResult bench_bus(const std::vector<Command>& cmds, double rate, int cpu_engine, int cpu_producer, int cpu_consumer) {
    namespace fs = std::filesystem;
    fs::path path = fs::temp_directory_path() / "bourse_bench_bus.dat";
    BusConfig cfg;
    cfg.n_in = 4;
    cfg.in_cap = 1 << 16;
    cfg.out_cap = 1 << 20;
    Bus bus;
    if (!bus.create(path.string(), cfg)) {
        std::fprintf(stderr, "cannot create %s\n", path.string().c_str());
        std::exit(1);
    }
    Engine eng(1 << 20);
    for (int s = 0; s < SYMBOLS; ++s) eng.add_symbol("S" + std::to_string(s), TICK);
    std::atomic<bool> stop{false};
    std::thread engine([&] {
        pin_thread(cpu_engine);
        run_loop(bus, eng, stop);
    });
    RingProducer prod;
    prod.attach(bus);
    LogConsumer cons;
    cons.attach(bus);
    size_t n = cmds.size();
    std::vector<uint64_t> sched(n), lat(n);
    std::atomic<bool> ready{false};
    uint64_t t_end = 0;
    std::thread reader([&] {
        pin_thread(cpu_consumer);
        Event buf[256];
        size_t next = 0;
        uint32_t ring = prod.index() << 28;
        uint64_t polls = 0;
        while (!ready.load(std::memory_order_acquire)) {
        }
        while (next < n) {
            size_t k = cons.poll(buf, 256);
            uint64_t now = __rdtsc();
            for (size_t i = 0; i < k; ++i)
                if (buf[i].cmd_ref == (ring | static_cast<uint32_t>(next & 0x0FFFFFFF))) {
                    lat[next] = now - sched[next];
                    ++next;
                }
            if ((++polls & 0xFFFF) == 0) cons.heartbeat();
        }
        t_end = __rdtsc();
    });
    pin_thread(cpu_producer);
    double period = rate > 0 ? g_tsc_per_ns * 1e9 / rate : 0;
    uint64_t t0 = __rdtsc() + static_cast<uint64_t>(g_tsc_per_ns * 1e6);  // start in 1 ms
    for (size_t i = 0; i < n; ++i) sched[i] = rate > 0 ? t0 + static_cast<uint64_t>(i * period) : 0;
    ready.store(true, std::memory_order_release);
    while (__rdtsc() < t0) {
    }
    for (size_t i = 0; i < n; ++i) {
        if (rate > 0)
            while (__rdtsc() < sched[i]) {
            }
        else
            sched[i] = __rdtsc();
        Command c = cmds[i];
        c.ts = sched[i];
        prod.push(c);
    }
    reader.join();
    stop = true;
    engine.join();
    pin_thread(-1);
    double secs = (t_end - (rate > 0 ? t0 : sched[0])) / g_tsc_per_ns / 1e9;
    return BusResult{rate, n / secs, percentiles(lat)};
}

}  // namespace

int main(int argc, char** argv) {
    size_t n = 4'000'000;
    std::string json_path;
    int ce = 2, cp = 4, cc = 6;
    for (int i = 1; i + 1 < argc; i += 2) {
        std::string a = argv[i];
        if (a == "--commands") n = std::strtoull(argv[i + 1], nullptr, 10);
        else if (a == "--json") json_path = argv[i + 1];
        else if (a == "--cpus") std::sscanf(argv[i + 1], "%d,%d,%d", &ce, &cp, &cc);
    }
    calibrate();
    std::printf("TSC: %.3f ticks/ns\n", g_tsc_per_ns);
    std::vector<Command> cmds = workload(n, 42);
    size_t news = 0, cancels = 0, replaces = 0;
    for (auto& c : cmds) (c.type == CMD_NEW ? news : c.type == CMD_CANCEL ? cancels : replaces)++;

    // 1. in process
    pin_thread(ce);
    uint64_t events = 0, executions = 0;
    {
        Engine e(1 << 20);
        for (int s = 0; s < SYMBOLS; ++s) e.add_symbol("S" + std::to_string(s), TICK);
        struct Count {
            uint64_t& n;
            uint64_t& x;
            void operator()(const Event& ev) {
                ++n;
                x += ev.type == EV_EXECUTED;
            }
        } cnt{events, executions};
        for (auto& c : cmds) e.process(c, cnt, 0);
    }
    double throughput;
    {
        Engine e(1 << 20);
        for (int s = 0; s < SYMBOLS; ++s) e.add_symbol("S" + std::to_string(s), TICK);
        Null sink;
        auto t0 = std::chrono::steady_clock::now();
        for (auto& c : cmds) e.process(c, sink, 0);
        throughput = n / std::chrono::duration<double>(std::chrono::steady_clock::now() - t0).count();
    }
    std::vector<uint64_t> per(n);
    {
        Engine e(1 << 20);
        for (int s = 0; s < SYMBOLS; ++s) e.add_symbol("S" + std::to_string(s), TICK);
        Null sink;
        for (size_t i = 0; i < n; ++i) {
            uint64_t a = __rdtsc();
            e.process(cmds[i], sink, 0);
            per[i] = __rdtsc() - a;
        }
    }
    Pct inproc = percentiles(per);
    std::printf("workload: %zu commands (%zu new, %zu cancel, %zu replace), %llu events, %llu executions\n", n, news, cancels, replaces,
                (unsigned long long)events, (unsigned long long)executions);
    std::printf("in process: %.2f M commands/s; per call ns %s\n", throughput / 1e6, pct_json(inproc).c_str());
    pin_thread(-1);

    // 2. over the bus
    std::vector<BusResult> rs;
    for (double rate : {100e3, 1e6, 2e6, 4e6, 0.0}) {
        rs.push_back(bench_bus(cmds, rate, ce, cp, cc));
        std::printf("bus, rate %s: achieved %.2f M/s; latency ns %s\n", rate > 0 ? std::to_string(static_cast<long long>(rate)).c_str() : "max",
                    rs.back().achieved / 1e6, pct_json(rs.back().lat).c_str());
        std::fflush(stdout);
    }
    if (!json_path.empty()) {
        std::FILE* f = std::fopen(json_path.c_str(), "w");
        std::fprintf(f, "{\n \"commands\": %zu,\n \"new\": %zu,\n \"cancel\": %zu,\n \"replace\": %zu,\n \"events\": %llu,\n \"executions\": %llu,\n", n,
                     news, cancels, replaces, (unsigned long long)events, (unsigned long long)executions);
        std::fprintf(f, " \"in_process\": {\"commands_per_second\": %.0f, \"latency_ns\": %s},\n \"bus\": [\n", throughput, pct_json(inproc).c_str());
        for (size_t i = 0; i < rs.size(); ++i)
            std::fprintf(f, "  {\"rate\": %.0f, \"achieved_per_second\": %.0f, \"latency_ns\": %s}%s\n", rs[i].rate, rs[i].achieved,
                         pct_json(rs[i].lat).c_str(), i + 1 < rs.size() ? "," : "");
        std::fprintf(f, " ]\n}\n");
        std::fclose(f);
    }
    return 0;
}
