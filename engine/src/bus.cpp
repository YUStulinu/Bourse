#include "bourse/bus.hpp"

#include <chrono>
#include <thread>

#ifdef _WIN32
#define WIN32_LEAN_AND_MEAN
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#else
#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>
#include <unistd.h>
#endif

namespace bourse {

uint64_t wall_ns() {
    return static_cast<uint64_t>(
        std::chrono::duration_cast<std::chrono::nanoseconds>(std::chrono::system_clock::now().time_since_epoch()).count());
}

uint32_t this_pid() {
#ifdef _WIN32
    return static_cast<uint32_t>(GetCurrentProcessId());
#else
    return static_cast<uint32_t>(getpid());
#endif
}

Mapping::~Mapping() {
#ifdef _WIN32
    if (p_) UnmapViewOfFile(p_);
    if (mh_) CloseHandle(mh_);
    if (fh_) CloseHandle(fh_);
#else
    if (p_) munmap(p_, n_);
    if (fd_ >= 0) close(fd_);
#endif
}

#ifdef _WIN32
static bool map_handle(HANDLE fh, size_t size, void*& mh, uint8_t*& p) {
    HANDLE m = CreateFileMappingA(fh, nullptr, PAGE_READWRITE, static_cast<DWORD>(uint64_t(size) >> 32), static_cast<DWORD>(size & 0xFFFFFFFFu), nullptr);
    if (!m) return false;
    void* v = MapViewOfFile(m, FILE_MAP_ALL_ACCESS, 0, 0, size);
    if (!v) {
        CloseHandle(m);
        return false;
    }
    mh = m;
    p = static_cast<uint8_t*>(v);
    return true;
}
#endif

bool Mapping::create(const std::string& path, size_t size) {
#ifdef _WIN32
    // a temporary file: the cache manager keeps it in memory instead of
    // writing its pages back to disk (which costs page faults later)
    HANDLE f = CreateFileA(path.c_str(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
                           CREATE_ALWAYS, FILE_ATTRIBUTE_TEMPORARY, nullptr);
    if (f == INVALID_HANDLE_VALUE) return false;
    fh_ = f;
    if (!map_handle(f, size, mh_, p_)) return false;
    std::memset(p_, 0, size);  // touch every page now, not on the hot path
    VirtualLock(p_, size);     // and keep them resident if the working set allows
#else
    fd_ = ::open(path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0644);
    if (fd_ < 0) return false;
    if (ftruncate(fd_, static_cast<off_t>(size)) != 0) return false;
    void* v = mmap(nullptr, size, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_POPULATE, fd_, 0);
    if (v == MAP_FAILED) return false;
    p_ = static_cast<uint8_t*>(v);
    std::memset(p_, 0, size);  // touch every page now, not on the hot path
#endif
    n_ = size;
    return true;
}

bool Mapping::open(const std::string& path) {
#ifdef _WIN32
    HANDLE f = CreateFileA(path.c_str(), GENERIC_READ | GENERIC_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE, nullptr,
                           OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
    if (f == INVALID_HANDLE_VALUE) return false;
    fh_ = f;
    LARGE_INTEGER sz;
    if (!GetFileSizeEx(f, &sz)) return false;
    n_ = static_cast<size_t>(sz.QuadPart);
    return map_handle(f, n_, mh_, p_);
#else
    fd_ = ::open(path.c_str(), O_RDWR);
    if (fd_ < 0) return false;
    struct stat st;
    if (fstat(fd_, &st) != 0) return false;
    n_ = static_cast<size_t>(st.st_size);
    void* v = mmap(nullptr, n_, PROT_READ | PROT_WRITE, MAP_SHARED, fd_, 0);
    if (v == MAP_FAILED) return false;
    p_ = static_cast<uint8_t*>(v);
    return true;
#endif
}

void Bus::layout() {
    out_off_ = RINGS_OFF + size_t(n_in_) * (RING_HEADER + size_t(in_cap_) * SLOT);
}

bool Bus::create(const std::string& path, const BusConfig& cfg) {
    if (!m_.create(path, total_size(cfg))) return false;
    uint8_t* b = base();
    std::memcpy(b + 0, &BUS_MAGIC, 8);
    std::memcpy(b + 8, &BUS_VERSION, 4);
    std::memcpy(b + 12, &cfg.n_in, 4);
    std::memcpy(b + 16, &cfg.in_cap, 4);
    std::memcpy(b + 20, &cfg.out_cap, 4);
    uint64_t t = wall_ns(), pid = this_pid();
    std::memcpy(b + 32, &t, 8);
    std::memcpy(b + 40, &pid, 8);
    n_in_ = cfg.n_in;
    in_cap_ = cfg.in_cap;
    out_cap_ = cfg.out_cap;
    layout();
    return true;
}

bool Bus::open(const std::string& path) {
    if (!m_.open(path) || m_.size() < RINGS_OFF) return false;
    uint64_t magic;
    std::memcpy(&magic, base(), 8);
    if (magic != BUS_MAGIC) return false;
    std::memcpy(&n_in_, base() + 12, 4);
    std::memcpy(&in_cap_, base() + 16, 4);
    std::memcpy(&out_cap_, base() + 20, 4);
    layout();
    return true;
}

void Bus::set_symbol(uint32_t i, const std::string& name, uint32_t round_lot, uint32_t tick) {
    SymbolEntry e;
    std::memset(e.name, ' ', 8);
    std::memcpy(e.name, name.data(), name.size() < 8 ? name.size() : 8);
    e.round_lot = round_lot;
    e.tick = tick;
    std::memcpy(base() + SYMBOLS_OFF + i * sizeof(SymbolEntry), &e, sizeof e);
}

SymbolEntry Bus::symbol(uint32_t i) const {
    SymbolEntry e;
    std::memcpy(&e, base() + SYMBOLS_OFF + i * sizeof(SymbolEntry), sizeof e);
    return e;
}

bool RingProducer::attach(Bus& bus) {
    for (uint32_t i = 0; i < bus.n_in(); ++i) {
        uint32_t expected = 0;
        if (bus.ring_owner(i).compare_exchange_strong(expected, this_pid())) {
            bus_ = &bus;
            i_ = i;
            head_ = bus.ring_head(i).load();
            tail_cache_ = bus.ring_tail(i).load();
            return true;
        }
    }
    return false;
}

void RingProducer::release() {
    if (bus_) bus_->ring_owner(i_).store(0);
}

bool LogConsumer::attach(Bus& bus) {
    for (uint32_t i = 0; i < MAX_CONSUMERS; ++i) {
        uint32_t expected = 0;
        if (bus.consumer_state(i).compare_exchange_strong(expected, 2)) {  // 2: registering
            bus_ = &bus;
            i_ = i;
            cursor_ = bus.log_cursor().load(std::memory_order_acquire);
            bus.consumer_cursor(i).store(cursor_);
            bus.consumer_heartbeat(i).store(wall_ns());
            uint32_t pid = this_pid();
            std::memcpy(bus.consumer(i) + 4, &pid, 4);
            bus.consumer_state(i).store(1, std::memory_order_release);
            return true;
        }
    }
    return false;
}

void LogConsumer::release() {
    if (bus_) bus_->consumer_state(i_).store(0);
}

uint64_t LogProducer::min_consumer() {
    uint64_t m = cursor_;
    for (uint32_t i = 0; i < MAX_CONSUMERS; ++i) {
        if (bus_.consumer_state(i).load(std::memory_order_acquire) != 1) continue;
        uint64_t c = bus_.consumer_cursor(i).load(std::memory_order_acquire);
        if (c < m) m = c;
    }
    return m;
}

void LogProducer::wait_for_room() {
    uint64_t spins = 0;
    while (true) {
        gate_ = min_consumer();
        if (cursor_ - gate_ < bus_.out_cap()) return;
        // readers are behind: let them catch up with what is published
        publish();
        if (++spins % 1024 == 0) {
            evict_stale(wall_ns(), 2'000'000'000ull);
            std::this_thread::yield();
        }
    }
}

void LogProducer::evict_stale(uint64_t now, uint64_t timeout_ns) {
    for (uint32_t i = 0; i < MAX_CONSUMERS; ++i) {
        if (bus_.consumer_state(i).load() != 1) continue;
        uint64_t hb = bus_.consumer_heartbeat(i).load(std::memory_order_relaxed);
        if (now > hb && now - hb > timeout_ns) {
            uint32_t one = 1;
            bus_.consumer_state(i).compare_exchange_strong(one, 0);
        }
    }
}

}  // namespace bourse
