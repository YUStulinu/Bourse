// The engine's main loop over the bus, shared by the engine process and the
// benchmark.
#pragma once

#include <atomic>
#include <cstdint>

#include "bus.hpp"
#include "engine.hpp"

namespace bourse {

/// Poll every inbound ring, match each command, write its events to the log,
/// until `stop` is set or the bus state becomes BUS_STOP. Returns the number
/// of commands processed.
uint64_t run_loop(Bus& bus, Engine& eng, const std::atomic<bool>& stop);

/// Pin the calling thread to one CPU (no effect when cpu < 0).
void pin_thread(int cpu);

}  // namespace bourse
