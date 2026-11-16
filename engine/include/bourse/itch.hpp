// NASDAQ TotalView-ITCH 5.0: message lengths and big-endian field readers.
// A file is a sequence of messages, each preceded by its length (u16, big
// endian). Every message starts with type (1), stock locate (2), tracking
// number (2) and a timestamp (6 bytes, nanoseconds since midnight).
#pragma once

#include <cstdint>
#include <cstdio>
#include <vector>

namespace bourse::itch {

inline uint16_t be16(const uint8_t* p) { return static_cast<uint16_t>((p[0] << 8) | p[1]); }
inline uint32_t be32(const uint8_t* p) { return (uint32_t(p[0]) << 24) | (uint32_t(p[1]) << 16) | (uint32_t(p[2]) << 8) | p[3]; }
inline uint64_t be48(const uint8_t* p) { return (uint64_t(be16(p)) << 32) | be32(p + 2); }
inline uint64_t be64(const uint8_t* p) { return (uint64_t(be32(p)) << 32) | be32(p + 4); }

/// The length of each message type in ITCH 5.0 (0 for unknown types).
inline int length_of(uint8_t type) {
    switch (type) {
        case 'S': return 12;
        case 'R': return 39;
        case 'H': return 25;
        case 'Y': return 20;
        case 'L': return 26;
        case 'V': return 35;
        case 'W': return 12;
        case 'K': return 28;
        case 'J': return 35;
        case 'h': return 21;
        case 'A': return 36;
        case 'F': return 40;
        case 'E': return 31;
        case 'C': return 36;
        case 'X': return 23;
        case 'D': return 19;
        case 'U': return 35;
        case 'P': return 44;
        case 'Q': return 40;
        case 'B': return 19;
        case 'I': return 50;
        case 'N': return 20;
        default: return 0;
    }
}

inline uint16_t locate(const uint8_t* m) { return be16(m + 1); }
inline uint64_t timestamp(const uint8_t* m) { return be48(m + 5); }

/// Reads a file of length-prefixed messages in large chunks.
class Reader {
public:
    explicit Reader(std::FILE* f, size_t chunk = size_t(64) << 20) : f_(f), buf_(chunk) {}

    /// Next message (pointer stays valid until the next call), or nullptr at
    /// the end; a message cut off by the end of the file is dropped.
    const uint8_t* next(uint16_t& len) {
        if (end_ - pos_ < 2 && !refill(2)) return nullptr;
        len = be16(&buf_[pos_]);
        if (end_ - pos_ < size_t(2) + len && !refill(size_t(2) + len)) return nullptr;
        const uint8_t* m = &buf_[pos_ + 2];
        pos_ += size_t(2) + len;
        bytes_ += size_t(2) + len;
        return m;
    }
    uint64_t bytes() const { return bytes_; }

private:
    std::FILE* f_;
    std::vector<uint8_t> buf_;
    size_t pos_ = 0, end_ = 0;
    uint64_t bytes_ = 0;

    bool refill(size_t need) {
        size_t rest = end_ - pos_;
        for (size_t i = 0; i < rest; ++i) buf_[i] = buf_[pos_ + i];
        pos_ = 0;
        end_ = rest;
        while (end_ < need) {
            size_t n = std::fread(&buf_[end_], 1, buf_.size() - end_, f_);
            if (n == 0) return false;
            end_ += n;
        }
        return true;
    }
};

}  // namespace bourse::itch
