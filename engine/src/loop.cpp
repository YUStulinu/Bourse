#include "bourse/loop.hpp"

#include <vector>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#else
#include <pthread.h>
#include <sched.h>
#endif

#if defined(__x86_64__) || defined(_M_X64)
#include <immintrin.h>
#define CPU_PAUSE() _mm_pause()
#else
#define CPU_PAUSE() ((void)0)
#endif

namespace bourse {

namespace {
struct LogSink {
    LogProducer& log;
    uint32_t cmd_ref = 0;
    void operator()(const Event& e) {
        Event x = e;
        x.cmd_ref = cmd_ref;
        log.write(x);
    }
};
}  // namespace

uint64_t run_loop(Bus& bus, Engine& eng, const std::atomic<bool>& stop) {
    LogProducer log(bus);
    LogSink sink{log};
    std::vector<uint64_t> tail(bus.n_in());
    for (uint32_t r = 0; r < bus.n_in(); ++r) tail[r] = bus.ring_tail(r).load();
    uint64_t commands = 0, iter = 0;
    while (!stop.load(std::memory_order_relaxed)) {
        bool any = false;
        for (uint32_t r = 0; r < bus.n_in(); ++r) {
            uint64_t h = bus.ring_head(r).load(std::memory_order_acquire);
            uint64_t t = tail[r];
            if (t == h) continue;
            any = true;
            for (; t < h; ++t) {
                sink.cmd_ref = (r << 28) | static_cast<uint32_t>(t & 0x0FFFFFFF);
                eng.process(*bus.ring_slot(r, t), sink, wall_ns());
                log.publish();
            }
            commands += h - tail[r];
            tail[r] = t;
            bus.ring_tail(r).store(t, std::memory_order_release);
        }
        if ((++iter & 0xFFFF) == 0) {
            uint64_t now = wall_ns();
            bus.heartbeat().store(now, std::memory_order_relaxed);
            log.evict_stale(now, 2'000'000'000ull);
            if (bus.state().load(std::memory_order_acquire) == BUS_STOP) break;
        }
        if (!any) CPU_PAUSE();
    }
    return commands;
}

void pin_thread(int cpu) {
    if (cpu < 0) return;
#ifdef _WIN32
    SetThreadAffinityMask(GetCurrentThread(), DWORD_PTR(1) << cpu);
#else
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(cpu, &set);
    pthread_setaffinity_np(pthread_self(), sizeof set, &set);
#endif
}

}  // namespace bourse
