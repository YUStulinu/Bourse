// Package book keeps order-by-order books built from ITCH messages, with
// price levels aggregated for depth snapshots.
package book

import (
	"sort"

	"bourse/marketdata/itch"
)

type order struct {
	locate uint16
	side   byte
	price  uint32
	shares uint32
}

// Level is one price with its total shares and number of orders.
type Level struct {
	Price  uint32 `json:"price"`
	Shares uint64 `json:"shares"`
	Orders int    `json:"orders"`
}

type side struct {
	levels map[uint32]*Level
	prices []uint32 // ascending
}

func (s *side) add(price, shares uint32) {
	l, ok := s.levels[price]
	if !ok {
		l = &Level{Price: price}
		s.levels[price] = l
		i := sort.Search(len(s.prices), func(i int) bool { return s.prices[i] >= price })
		s.prices = append(s.prices, 0)
		copy(s.prices[i+1:], s.prices[i:])
		s.prices[i] = price
	}
	l.Shares += uint64(shares)
	l.Orders++
}

func (s *side) reduce(price, shares uint32, gone bool) {
	l := s.levels[price]
	if l == nil {
		return
	}
	l.Shares -= uint64(shares)
	if gone {
		l.Orders--
	}
	if l.Orders == 0 {
		delete(s.levels, price)
		i := sort.Search(len(s.prices), func(i int) bool { return s.prices[i] >= price })
		if i < len(s.prices) && s.prices[i] == price {
			s.prices = append(s.prices[:i], s.prices[i+1:]...)
		}
	}
}

// Trade is a print: an execution against a displayed order, or a hidden one.
type Trade struct {
	Timestamp uint64 `json:"ts"`
	Price     uint32 `json:"price"`
	Shares    uint32 `json:"shares"`
	Aggressor byte   `json:"-"`
	Side      string `json:"side"` // the aggressor: "B" or "S"
}

// Book of one stock.
type Book struct {
	Name     string
	Locate   uint16
	RoundLot uint32
	State    byte
	bids     side
	asks     side
	Volume   uint64
	Last     uint32
	Trades   uint64
}

func newBook(locate uint16) *Book {
	return &Book{Locate: locate, State: 'T', bids: side{levels: map[uint32]*Level{}}, asks: side{levels: map[uint32]*Level{}}}
}

func (b *Book) side(s byte) *side {
	if s == 'B' {
		return &b.bids
	}
	return &b.asks
}

// Best bid and ask (0 when a side is empty).
func (b *Book) Best() (bid uint32, bidShares uint64, ask uint32, askShares uint64) {
	if n := len(b.bids.prices); n > 0 {
		bid = b.bids.prices[n-1]
		bidShares = b.bids.levels[bid].Shares
	}
	if len(b.asks.prices) > 0 {
		ask = b.asks.prices[0]
		askShares = b.asks.levels[ask].Shares
	}
	return
}

// Depth: the best n levels of each side, best first.
func (b *Book) Depth(n int) (bids, asks []Level) {
	for i := len(b.bids.prices) - 1; i >= 0 && len(bids) < n; i-- {
		bids = append(bids, *b.bids.levels[b.bids.prices[i]])
	}
	for i := 0; i < len(b.asks.prices) && len(asks) < n; i++ {
		asks = append(asks, *b.asks.levels[b.asks.prices[i]])
	}
	return
}

// Stats counts what a correct feed never contains.
type Stats struct {
	Messages        uint64
	UnknownRefs     uint64
	OverExecuted    uint64
	CrossedChecks   uint64
	Crossed         uint64
	Locked          uint64
	ExecutedShares  uint64
	HiddenShares    uint64
	OrdersAdded     uint64
}

// Books holds every stock's book and the live orders.
type Books struct {
	orders     map[uint64]order
	books      []*Book
	byName     map[string]*Book
	marketOpen bool
	Stats      Stats
	// OnTrade, if set, is called for every print
	OnTrade func(b *Book, t Trade)
	// OnChange, if set, is called when a book changes
	OnChange func(b *Book)
}

func New() *Books {
	return &Books{orders: make(map[uint64]order, 1<<20), byName: map[string]*Book{}}
}

func (bs *Books) book(locate uint16) *Book {
	for int(locate) >= len(bs.books) {
		bs.books = append(bs.books, nil)
	}
	if bs.books[locate] == nil {
		bs.books[locate] = newBook(locate)
	}
	return bs.books[locate]
}

// Get a book by stock name (nil if unknown).
func (bs *Books) Get(name string) *Book { return bs.byName[name] }

// All books that have a name.
func (bs *Books) All() []*Book {
	var out []*Book
	for _, b := range bs.books {
		if b != nil && b.Name != "" {
			out = append(out, b)
		}
	}
	return out
}

func (bs *Books) LiveOrders() int { return len(bs.orders) }

func (bs *Books) checkCross(b *Book) {
	if !bs.marketOpen || b.State != 'T' {
		return
	}
	bs.Stats.CrossedChecks++
	bid, _, ask, _ := b.Best()
	if bid == 0 || ask == 0 {
		return
	}
	if bid > ask {
		bs.Stats.Crossed++
	} else if bid == ask {
		bs.Stats.Locked++
	}
}

func (bs *Books) take(ref uint64, shares uint32, all bool) (*Book, order, bool) {
	o, ok := bs.orders[ref]
	if !ok {
		bs.Stats.UnknownRefs++
		return nil, o, false
	}
	if all {
		shares = o.shares
	}
	if shares > o.shares {
		bs.Stats.OverExecuted++
		shares = o.shares
	}
	b := bs.books[o.locate]
	gone := shares == o.shares
	b.side(o.side).reduce(o.price, shares, gone)
	if gone {
		delete(bs.orders, ref)
	} else {
		o.shares -= shares
		bs.orders[ref] = o
	}
	return b, o, true
}

// Apply one message.
func (bs *Books) Apply(m *itch.Msg) {
	bs.Stats.Messages++
	switch m.Type {
	case 'S':
		if m.Code == 'Q' {
			bs.marketOpen = true
		} else if m.Code == 'M' {
			bs.marketOpen = false
		}
	case 'R':
		b := bs.book(m.Locate)
		b.Name = m.Stock
		b.RoundLot = m.RoundLot
		bs.byName[m.Stock] = b
	case 'H':
		bs.book(m.Locate).State = m.Code
	case 'A', 'F':
		b := bs.book(m.Locate)
		bs.orders[m.Ref] = order{m.Locate, m.Side, m.Price, m.Shares}
		b.side(m.Side).add(m.Price, m.Shares)
		bs.Stats.OrdersAdded++
		bs.checkCross(b)
		if bs.OnChange != nil {
			bs.OnChange(b)
		}
	case 'E', 'C':
		b, o, ok := bs.take(m.Ref, m.Shares, false)
		if !ok {
			return
		}
		bs.Stats.ExecutedShares += uint64(m.Shares)
		price := o.price
		if m.Type == 'C' {
			price = m.Price
		}
		if m.Type == 'E' || m.Printable {
			agg := byte('B')
			if o.side == 'B' {
				agg = 'S'
			}
			bs.trade(b, Trade{m.Timestamp, price, m.Shares, agg, string(agg)})
		}
		if bs.OnChange != nil {
			bs.OnChange(b)
		}
	case 'X':
		if b, _, ok := bs.take(m.Ref, m.Shares, false); ok && bs.OnChange != nil {
			bs.OnChange(b)
		}
	case 'D':
		if b, _, ok := bs.take(m.Ref, 0, true); ok && bs.OnChange != nil {
			bs.OnChange(b)
		}
	case 'U':
		b, o, ok := bs.take(m.Ref, 0, true)
		if !ok {
			return
		}
		bs.orders[m.NewRef] = order{o.locate, o.side, m.Price, m.Shares}
		b.side(o.side).add(m.Price, m.Shares)
		bs.checkCross(b)
		if bs.OnChange != nil {
			bs.OnChange(b)
		}
	case 'P':
		b := bs.book(m.Locate)
		bs.Stats.HiddenShares += uint64(m.Shares)
		agg := byte('B')
		if m.Side == 'B' {
			agg = 'S'
		}
		bs.trade(b, Trade{m.Timestamp, m.Price, m.Shares, agg, string(agg)})
	}
}

func (bs *Books) trade(b *Book, t Trade) {
	b.Volume += uint64(t.Shares)
	b.Last = t.Price
	b.Trades++
	if bs.OnTrade != nil {
		bs.OnTrade(b, t)
	}
}
