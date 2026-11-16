// Commands into the engine and events out of it: the 64-byte records of the
// shared-memory bus (docs/bus.md).
#pragma once

#include <cstddef>
#include <cstdint>

namespace bourse {

enum CmdType : uint8_t { CMD_NEW = 1, CMD_CANCEL = 2, CMD_REPLACE = 3, CMD_MASS_CANCEL = 4, CMD_BLOCK = 5, CMD_UNBLOCK = 6 };
enum Tif : uint8_t { TIF_DAY = 0, TIF_IOC = 1, TIF_FOK = 2 };
enum CmdFlags : uint8_t { F_POST_ONLY = 1, F_MARKET = 2, F_STP = 4 };

enum EvType : uint8_t {
    EV_ACCEPTED = 1,
    EV_REJECTED = 2,
    EV_EXECUTED = 3,
    EV_CANCELLED = 4,
    EV_REPLACED = 5,
    EV_BOOKED = 6,
    EV_BLOCKED = 7,
    EV_UNBLOCKED = 8,
};

enum RejectReason : uint8_t {
    REJ_SYMBOL = 1,
    REJ_QTY = 2,
    REJ_PRICE = 3,
    REJ_DUPLICATE = 4,
    REJ_UNKNOWN_ORDER = 5,
    REJ_BLOCKED = 6,
    REJ_POST_ONLY = 7,
    REJ_HALTED = 8,
};

enum CancelReason : uint8_t {
    CXL_USER = 1,
    CXL_IOC = 2,
    CXL_STP = 3,
    CXL_MASS = 4,
    CXL_FOK = 5,
    CXL_BLOCKED = 6,
};

constexpr uint32_t ALL_SYMBOLS = 0xFFFFFFFFu;

struct Command {
    uint8_t type;
    uint8_t side;  // 'B' or 'S'
    uint8_t tif;
    uint8_t flags;
    uint16_t session;
    uint16_t reserved0;
    uint32_t symbol;
    uint32_t account;
    uint64_t order_id;
    uint64_t new_order_id;
    int64_t price;
    uint32_t qty;
    uint32_t reserved1;
    uint64_t reserved2;
    uint64_t ts;
};

struct Event {
    uint8_t type;
    uint8_t side;
    uint8_t reason;
    uint8_t flags;
    uint16_t session;
    uint16_t session2;
    uint32_t symbol;
    uint32_t account;
    uint64_t order_id;
    uint64_t order_id2;
    int64_t price;
    uint32_t qty;
    uint32_t qty2;
    uint32_t account2;
    uint32_t cmd_ref;  // which command produced this event: ring index << 28 | ring position mod 2^28
    uint64_t ts;
};

static_assert(sizeof(Command) == 64);
static_assert(sizeof(Event) == 64);
static_assert(offsetof(Command, order_id) == 16 && offsetof(Command, price) == 32 && offsetof(Command, qty) == 40 && offsetof(Command, ts) == 56);
static_assert(offsetof(Event, order_id) == 16 && offsetof(Event, price) == 32 && offsetof(Event, qty) == 40 && offsetof(Event, account2) == 48 && offsetof(Event, ts) == 56);

}  // namespace bourse
