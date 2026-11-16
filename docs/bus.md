# The shared-memory bus

Every component of Bourse talks to the matching engine through one file mapped
into memory (`run/bus.dat` by default). The engine creates it; the gateways
(Rust), the market-data service (Go) and the risk and clearing service
(Kotlin) map the same file. There are no locks anywhere: every queue has
exactly one writer, and readers only ever read the writer's cursor.

- **Inbound rings**, one per producer (a gateway process, the risk service):
  single-producer single-consumer queues of 64-byte commands. The engine is
  the only consumer of all of them.
- **The event log**: one single-producer multi-consumer ring of 64-byte
  events, written by the engine in the order things happened. Every consumer
  sees every event (a broadcast, like the LMAX Disruptor). Consumers register
  in a table and publish how far they have read; the engine never overwrites
  an event a registered consumer has not read yet. A consumer that stops
  updating its heartbeat for 2 seconds is dropped, so a crashed reader cannot
  stall the exchange.

All integers are little-endian. A cursor is a 64-bit count of slots written
(or read) since the start; the slot of cursor `c` is `c mod capacity`, and
capacities are powers of two. Writers publish a cursor with release
semantics after filling the slot, and readers load it with acquire semantics
before reading the slot.

## Layout

| offset | size | content |
|---:|---:|---|
| 0 | 4096 | header |
| 4096 | 4096 | symbol table: 256 entries of 16 bytes |
| 8192 | 4096 | consumer table: 16 entries of 128 bytes (2048 bytes used) |
| 12288 | `n_in * (256 + in_cap * 64)` | inbound rings |
| after the rings | `256 + out_cap * 64` | event log |

Header:

| offset | type | field |
|---:|---|---|
| 0 | u64 | magic, the bytes `BOURSEB1` |
| 8 | u32 | version (1) |
| 12 | u32 | `n_in`, number of inbound rings (16) |
| 16 | u32 | `in_cap`, slots per inbound ring |
| 20 | u32 | `out_cap`, slots in the event log |
| 24 | u32 | number of symbols |
| 28 | u32 | state: 0 starting, 1 running, 2 stop requested, 3 stopped |
| 32 | u64 | engine start time, nanoseconds since the Unix epoch |
| 40 | u64 | engine process id |
| 64 | u64 | engine heartbeat, nanoseconds since the Unix epoch |

Symbol entry: `name` (8 bytes, ASCII, padded with spaces as in ITCH),
`round_lot` (u32), `tick` (u32, in units of 1/10000 dollar). The index of a
symbol in the table is its id everywhere.

Consumer entry: `state` (u32 at 0: 0 free, 1 active, 2 registering), `pid`
(u32 at 4), `heartbeat` (u64 at 8, ns since the epoch), `cursor` (u64 at 64,
the number of events read). To register, a consumer changes a free entry's
state from 0 to 2 with compare-and-swap, sets its cursor to the event log's
current cursor and its heartbeat to now, then sets the state to 1.

Inbound ring: `head` (u64 at 0, written by the producer), `owner` (u32 at 8:
0 free, otherwise the producer's process id, claimed with compare-and-swap),
`tail` (u64 at 128, written by the engine), slots from 256.

Event log: `cursor` (u64 at 0), slots from 256.

## Commands (64 bytes)

| offset | type | field |
|---:|---|---|
| 0 | u8 | type: 1 new order, 2 cancel, 3 replace, 4 mass cancel, 5 block account, 6 unblock account |
| 1 | u8 | side: `B` or `S` |
| 2 | u8 | time in force: 0 day, 1 immediate-or-cancel, 2 fill-or-kill |
| 3 | u8 | flags: 1 post only, 2 market order (no limit), 4 self-trade prevention (cancel the resting order) |
| 4 | u16 | session |
| 6 | u16 | reserved |
| 8 | u32 | symbol (mass cancel: `0xFFFFFFFF` for all) |
| 12 | u32 | account |
| 16 | u64 | order id (new: the new id; cancel, replace: the existing order) |
| 24 | u64 | replace: the new order id |
| 32 | i64 | price, 1/10000 dollar |
| 40 | u32 | quantity (new, replace: the open quantity; cancel: the quantity to leave open, 0 cancels all) |
| 44 | u32 | reserved |
| 48 | u64 | reserved |
| 56 | u64 | the producer's timestamp (ns since the epoch), for latency measurement |

Order ids are chosen by the producer and must be unique among live orders:
gateways use `(session << 40) | counter`.

## Events (64 bytes)

| offset | type | field |
|---:|---|---|
| 0 | u8 | type (below) |
| 1 | u8 | side of the order (executed: of the resting order) |
| 2 | u8 | reason |
| 3 | u8 | flags (executed: 1 when the resting order is now filled) |
| 4 | u16 | session of the order (executed: of the resting order) |
| 6 | u16 | executed: session of the incoming order |
| 8 | u32 | symbol |
| 12 | u32 | account (executed: of the resting order) |
| 16 | u64 | order id (executed: the resting order) |
| 24 | u64 | executed: the incoming order; replaced: the new id |
| 32 | i64 | price (executed: the execution price) |
| 40 | u32 | quantity (accepted, booked, replaced: open quantity; executed: executed quantity; cancelled: quantity cancelled) |
| 44 | u32 | cancelled: quantity still open; executed: quantity of the incoming order still unfilled; replaced: open quantity of the old order |
| 48 | u32 | executed: account of the incoming order |
| 52 | u32 | the command this event comes from: inbound ring index × 2^28 + (position in that ring mod 2^28) |
| 56 | u64 | engine timestamp, ns since the epoch |

| type | event | notes |
|---:|---|---|
| 1 | accepted | the order passed validation; executions may follow (reason holds the time in force, flags the order's flags) |
| 2 | rejected | reasons: 1 unknown symbol, 2 bad quantity, 3 bad price, 4 duplicate id, 5 unknown order, 6 account blocked, 7 post-only order would execute, 8 halted |
| 3 | executed | one fill between a resting and an incoming order, at the resting order's price; the match number is the event's sequence number |
| 4 | cancelled | reasons: 1 by the user, 2 rest of an immediate-or-cancel order, 3 self-trade prevention, 4 mass cancel, 5 fill-or-kill not filled, 6 account blocked |
| 5 | replaced | the old order is gone, the new one follows as an incoming order |
| 6 | booked | the open quantity of an order now rests on the book (market data shows it from here) |
| 7 | account blocked | |
| 8 | account unblocked | |

For one incoming order the engine writes: accepted (or replaced, or rejected),
then any executions, then booked or cancelled for what is left.
