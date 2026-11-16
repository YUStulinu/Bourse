// bourse-engine: the matching engine process. It creates the bus, then loops
// over the inbound rings on one thread, matching every command and writing
// the events to the event log.
//
//   bourse-engine --bus run/bus.dat --symbols config/symbols.txt [--cpu N]
#include <atomic>
#include <csignal>
#include <cstdio>
#include <cstdlib>
#include <fstream>
#include <sstream>
#include <string>
#include <vector>

#include "bourse/bus.hpp"
#include "bourse/engine.hpp"
#include "bourse/loop.hpp"

using namespace bourse;

static std::atomic<bool> g_stop{false};
static void on_signal(int) { g_stop.store(true); }

int main(int argc, char** argv) {
    std::string bus_path = "run/bus.dat", sym_path = "config/symbols.txt";
    BusConfig cfg;
    int cpu = -1;
    for (int i = 1; i + 1 < argc; i += 2) {
        std::string a = argv[i];
        if (a == "--bus") bus_path = argv[i + 1];
        else if (a == "--symbols") sym_path = argv[i + 1];
        else if (a == "--in-cap") cfg.in_cap = static_cast<uint32_t>(std::strtoul(argv[i + 1], nullptr, 10));
        else if (a == "--out-cap") cfg.out_cap = static_cast<uint32_t>(std::strtoul(argv[i + 1], nullptr, 10));
        else if (a == "--cpu") cpu = std::atoi(argv[i + 1]);
        else {
            std::fprintf(stderr, "unknown option %s\n", a.c_str());
            return 2;
        }
    }
    struct Sym {
        std::string name;
        uint32_t lot, tick;
    };
    std::vector<Sym> syms;
    std::ifstream f(sym_path);
    for (std::string line; std::getline(f, line);) {
        if (line.empty() || line[0] == '#') continue;
        std::istringstream ss(line);
        Sym s{"", 100, 100};
        ss >> s.name >> s.lot >> s.tick;
        if (!s.name.empty()) syms.push_back(s);
    }
    if (syms.empty() || syms.size() > MAX_SYMBOLS) {
        std::fprintf(stderr, "no symbols in %s\n", sym_path.c_str());
        return 2;
    }

    Bus bus;
    if (!bus.create(bus_path, cfg)) {
        std::fprintf(stderr, "cannot create %s\n", bus_path.c_str());
        return 1;
    }
    Engine eng(1 << 20);
    for (uint32_t i = 0; i < syms.size(); ++i) {
        bus.set_symbol(i, syms[i].name, syms[i].lot, syms[i].tick);
        eng.add_symbol(syms[i].name, syms[i].tick);
    }
    bus.set_n_symbols(static_cast<uint32_t>(syms.size()));
    std::signal(SIGINT, on_signal);
    std::signal(SIGTERM, on_signal);
    pin_thread(cpu);
    bus.heartbeat().store(wall_ns());
    bus.state().store(BUS_RUNNING, std::memory_order_release);
    std::printf("bourse-engine: %zu symbols, bus %s (%u rings of %u, log of %u)\n", syms.size(), bus_path.c_str(), cfg.n_in, cfg.in_cap,
                cfg.out_cap);
    std::fflush(stdout);
    uint64_t commands = run_loop(bus, eng, g_stop);
    bus.state().store(BUS_STOPPED, std::memory_order_release);
    std::printf("bourse-engine: stopped after %llu commands, %llu events\n", static_cast<unsigned long long>(commands),
                static_cast<unsigned long long>(bus.log_cursor().load()));
    return 0;
}
