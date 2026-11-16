// The shared-memory bus (docs/bus.md): a mapped file holding the inbound
// single-producer single-consumer rings and the broadcast event log.
#pragma once

#include <atomic>
#include <cstdint>
#include <cstring>
#include <string>

#include "protocol.hpp"

namespace bourse {

constexpr uint64_t BUS_MAGIC = 0x3142455352554F42ull;  // "BOURSEB1"
constexpr uint32_t BUS_VERSION = 1;
constexpr size_t HEADER_OFF = 0, SYMBOLS_OFF = 4096, CONSUMERS_OFF = 8192, RINGS_OFF = 12288;
constexpr uint32_t MAX_SYMBOLS = 256, MAX_CONSUMERS = 16;
constexpr size_t CONSUMER_SIZE = 128, RING_HEADER = 256, SLOT = 64;

enum BusState : uint32_t { BUS_STARTING = 0, BUS_RUNNING = 1, BUS_STOP = 2, BUS_STOPPED = 3 };

uint64_t wall_ns();  // nanoseconds since the Unix epoch
uint32_t this_pid();

template <class T>
inline std::atomic_ref<T> at(uint8_t* base, size_t off) {
    return std::atomic_ref<T>(*reinterpret_cast<T*>(base + off));
}

/// A file mapped into memory (created and sized, or opened).
class Mapping {
public:
    Mapping() = default;
    Mapping(const Mapping&) = delete;
    Mapping& operator=(const Mapping&) = delete;
    ~Mapping();
    bool create(const std::string& path, size_t size);
    bool open(const std::string& path);
    uint8_t* data() const { return p_; }
    size_t size() const { return n_; }

private:
    uint8_t* p_ = nullptr;
    size_t n_ = 0;
    void* fh_ = nullptr;  // platform handles
    void* mh_ = nullptr;
    [[maybe_unused]] int fd_ = -1;
};

struct BusConfig {
    uint32_t n_in = 16;
    uint32_t in_cap = 1 << 16;
    uint32_t out_cap = 1 << 20;
};

struct SymbolEntry {
    char name[8];
    uint32_t round_lot;
    uint32_t tick;
};
static_assert(sizeof(SymbolEntry) == 16);

class Bus {
public:
    /// Create the bus file (the engine does this).
    bool create(const std::string& path, const BusConfig& cfg);
    /// Map an existing bus file.
    bool open(const std::string& path);

    uint8_t* base() const { return m_.data(); }
    uint32_t n_in() const { return n_in_; }
    uint32_t in_cap() const { return in_cap_; }
    uint32_t out_cap() const { return out_cap_; }

    std::atomic_ref<uint32_t> state() { return at<uint32_t>(base(), 28); }
    std::atomic_ref<uint64_t> heartbeat() { return at<uint64_t>(base(), 64); }
    void set_symbol(uint32_t i, const std::string& name, uint32_t round_lot, uint32_t tick);
    SymbolEntry symbol(uint32_t i) const;
    uint32_t n_symbols() const { return *reinterpret_cast<const uint32_t*>(base() + 24); }
    void set_n_symbols(uint32_t n) { *reinterpret_cast<uint32_t*>(base() + 24) = n; }

    // inbound rings
    uint8_t* ring(uint32_t i) const { return base() + RINGS_OFF + i * (RING_HEADER + size_t(in_cap_) * SLOT); }
    std::atomic_ref<uint64_t> ring_head(uint32_t i) { return at<uint64_t>(ring(i), 0); }
    std::atomic_ref<uint32_t> ring_owner(uint32_t i) { return at<uint32_t>(ring(i), 8); }
    std::atomic_ref<uint64_t> ring_tail(uint32_t i) { return at<uint64_t>(ring(i), 128); }
    Command* ring_slot(uint32_t i, uint64_t c) const {
        return reinterpret_cast<Command*>(ring(i) + RING_HEADER + (c & (in_cap_ - 1)) * SLOT);
    }

    // event log
    uint8_t* log() const { return base() + out_off_; }
    std::atomic_ref<uint64_t> log_cursor() { return at<uint64_t>(log(), 0); }
    Event* log_slot(uint64_t c) const { return reinterpret_cast<Event*>(log() + RING_HEADER + (c & (out_cap_ - 1)) * SLOT); }

    // consumers of the event log
    uint8_t* consumer(uint32_t i) const { return base() + CONSUMERS_OFF + i * CONSUMER_SIZE; }
    std::atomic_ref<uint32_t> consumer_state(uint32_t i) { return at<uint32_t>(consumer(i), 0); }
    std::atomic_ref<uint64_t> consumer_heartbeat(uint32_t i) { return at<uint64_t>(consumer(i), 8); }
    std::atomic_ref<uint64_t> consumer_cursor(uint32_t i) { return at<uint64_t>(consumer(i), 64); }

    static size_t total_size(const BusConfig& c) {
        return RINGS_OFF + size_t(c.n_in) * (RING_HEADER + size_t(c.in_cap) * SLOT) + RING_HEADER + size_t(c.out_cap) * SLOT;
    }

private:
    Mapping m_;
    uint32_t n_in_ = 0, in_cap_ = 0, out_cap_ = 0;
    size_t out_off_ = 0;
    void layout();
};

/// Producer side of one inbound ring (a gateway, a test, the benchmark).
class RingProducer {
public:
    /// Claim a free ring; returns false when all are taken.
    bool attach(Bus& bus);
    /// Use ring i without claiming it (tests).
    void attach(Bus& bus, uint32_t i) {
        bus_ = &bus;
        i_ = i;
    }
    void release();
    /// Spin until there is room, then publish one command.
    void push(const Command& c) {
        uint64_t h = head_;
        while (h - tail_cache_ >= bus_->in_cap()) tail_cache_ = bus_->ring_tail(i_).load(std::memory_order_acquire);
        *bus_->ring_slot(i_, h) = c;
        head_ = h + 1;
        bus_->ring_head(i_).store(head_, std::memory_order_release);
    }
    uint32_t index() const { return i_; }

private:
    Bus* bus_ = nullptr;
    uint32_t i_ = 0;
    uint64_t head_ = 0, tail_cache_ = 0;
};

/// A registered reader of the event log.
class LogConsumer {
public:
    bool attach(Bus& bus);
    void release();
    /// Copy up to max new events into out; returns how many.
    size_t poll(Event* out, size_t max) {
        uint64_t avail = bus_->log_cursor().load(std::memory_order_acquire);
        size_t n = 0;
        while (cursor_ < avail && n < max) out[n++] = *bus_->log_slot(cursor_++);
        if (n) bus_->consumer_cursor(i_).store(cursor_, std::memory_order_release);
        return n;
    }
    void heartbeat() { bus_->consumer_heartbeat(i_).store(wall_ns(), std::memory_order_relaxed); }
    uint64_t cursor() const { return cursor_; }

private:
    Bus* bus_ = nullptr;
    uint32_t i_ = 0;
    uint64_t cursor_ = 0;
};

/// The engine's side of the event log: waits for registered consumers.
class LogProducer {
public:
    explicit LogProducer(Bus& bus) : bus_(bus), cursor_(bus.log_cursor().load()) {}
    /// Write one event (not yet visible to readers).
    void write(const Event& e) {
        if (cursor_ - gate_ >= bus_.out_cap()) wait_for_room();
        *bus_.log_slot(cursor_) = e;
        ++cursor_;
    }
    /// Make everything written so far visible.
    void publish() { bus_.log_cursor().store(cursor_, std::memory_order_release); }
    uint64_t cursor() const { return cursor_; }
    /// Drop consumers whose heartbeat is older than timeout_ns.
    void evict_stale(uint64_t now, uint64_t timeout_ns);

private:
    Bus& bus_;
    uint64_t cursor_;
    uint64_t gate_ = 0;  // all active consumers have read up to here
    void wait_for_room();
    uint64_t min_consumer();
};

}  // namespace bourse
