// Package md is the market-data distributor: it takes the ITCH stream of
// the exchange, keeps the books, and fans the stream out to many
// subscribers: MoldUDP64 over UDP (with retransmission on request), and
// conflated book snapshots and trades as JSON over WebSocket for browsers.
package md

import (
	"encoding/json"
	"log"
	"net"
	"net/http"
	"sort"
	"strconv"
	"sync"
	"sync/atomic"
	"time"

	"bourse/marketdata/book"
	"bourse/marketdata/itch"
	"bourse/marketdata/mold"
	"bourse/marketdata/ws"
)

const nsPerDay = 86_400_000_000_000

// Latency keeps a window of samples (ns) for percentiles.
type Latency struct {
	mu      sync.Mutex
	samples []int64
	next    int
	count   uint64
}

func (l *Latency) Add(ns int64) {
	l.mu.Lock()
	if len(l.samples) < 1<<20 {
		l.samples = append(l.samples, ns)
	} else {
		l.samples[l.next] = ns
		l.next = (l.next + 1) % len(l.samples)
	}
	l.count++
	l.mu.Unlock()
}

// Percentiles in microseconds: p50, p99, p99.9, max.
func (l *Latency) Percentiles() map[string]float64 {
	l.mu.Lock()
	v := append([]int64(nil), l.samples...)
	l.mu.Unlock()
	if len(v) == 0 {
		return nil
	}
	sort.Slice(v, func(i, j int) bool { return v[i] < v[j] })
	at := func(q float64) float64 {
		i := int(q * float64(len(v)))
		if i >= len(v) {
			i = len(v) - 1
		}
		return float64(v[i]) / 1000
	}
	return map[string]float64{"p50": at(0.5), "p99": at(0.99), "p99_9": at(0.999), "max": float64(v[len(v)-1]) / 1000}
}

// NowOfDay: nanoseconds since midnight UTC, the clock of ITCH timestamps.
func NowOfDay() int64 { return WallNanos() % nsPerDay }

type wsClient struct {
	conn   *ws.Conn
	mu     sync.Mutex
	symbol string
	dirty  bool
	trades []book.Trade
	wake   chan struct{}
}

type Server struct {
	mu      sync.RWMutex // guards books
	books   *book.Books
	store   mold.Store
	batch   *mold.Batcher
	session [10]byte
	udp     *net.UDPConn
	subs    atomic.Value // []*net.UDPAddr
	subsMu  sync.Mutex

	wsMu    sync.Mutex
	clients map[*wsClient]bool

	FeedLatency Latency
	Messages    atomic.Uint64
	Packets     atomic.Uint64
	Retransmits atomic.Uint64
}

func New(session string) *Server {
	s := &Server{books: book.New(), session: mold.Session(session), clients: map[*wsClient]bool{}}
	s.batch = mold.NewBatcher(s.session)
	s.subs.Store([]*net.UDPAddr{})
	s.books.OnChange = s.onChange
	s.books.OnTrade = s.onTrade
	return s
}

// ListenUDP opens the MoldUDP64 socket: subscribers send "JOIN" to it to
// receive the stream, and retransmission requests.
func (s *Server) ListenUDP(addr string) error {
	a, err := net.ResolveUDPAddr("udp", addr)
	if err != nil {
		return err
	}
	c, err := net.ListenUDP("udp", a)
	if err != nil {
		return err
	}
	c.SetWriteBuffer(4 << 20)
	c.SetReadBuffer(4 << 20)
	s.udp = c
	go s.serveUDP()
	go s.heartbeats()
	return nil
}

func (s *Server) UDPAddr() string { return s.udp.LocalAddr().String() }

func (s *Server) serveUDP() {
	buf := make([]byte, 2048)
	for {
		n, from, err := s.udp.ReadFromUDP(buf)
		if err != nil {
			return
		}
		p := buf[:n]
		switch {
		case string(p) == "JOIN":
			s.subsMu.Lock()
			old := s.subs.Load().([]*net.UDPAddr)
			found := false
			for _, a := range old {
				if a.String() == from.String() {
					found = true
				}
			}
			if !found {
				s.subs.Store(append(append([]*net.UDPAddr(nil), old...), from))
			}
			s.subsMu.Unlock()
		case string(p) == "LEAVE":
			s.subsMu.Lock()
			old := s.subs.Load().([]*net.UDPAddr)
			var keep []*net.UDPAddr
			for _, a := range old {
				if a.String() != from.String() {
					keep = append(keep, a)
				}
			}
			s.subs.Store(keep)
			s.subsMu.Unlock()
		case n == mold.HeaderLen:
			_, seq, _, _ := mold.Parse(p)
			count := int(p[18])<<8 | int(p[19])
			msgs := s.store.Get(seq, count)
			if len(msgs) > 0 {
				s.udp.WriteToUDP(mold.Packet(s.session, seq, msgs), from)
				s.Retransmits.Add(uint64(len(msgs)))
			}
		}
	}
}

func (s *Server) heartbeats() {
	for range time.Tick(time.Second) {
		s.send(mold.Heartbeat(s.session, s.store.Next()))
	}
}

func (s *Server) send(pkt []byte) {
	if s.udp == nil || pkt == nil {
		return
	}
	for _, a := range s.subs.Load().([]*net.UDPAddr) {
		s.udp.WriteToUDP(pkt, a)
	}
	s.Packets.Add(1)
}

// Subscribers on UDP.
func (s *Server) Subscribers() int { return len(s.subs.Load().([]*net.UDPAddr)) }

// Publish one ITCH message; flush=true sends the pending packet now.
func (s *Server) Publish(raw []byte, flush bool) {
	m := append([]byte(nil), raw...)
	var msg itch.Msg
	if err := itch.Decode(m, &msg); err == nil {
		switch msg.Type {
		case 'A', 'F', 'E', 'C', 'X', 'D', 'U', 'P':
			s.FeedLatency.Add((NowOfDay() - int64(msg.Timestamp) + nsPerDay) % nsPerDay)
		}
		s.mu.Lock()
		s.books.Apply(&msg)
		s.mu.Unlock()
	}
	seq := s.store.Append(m)
	s.Messages.Add(1)
	if pkt := s.batch.Add(seq, m); pkt != nil {
		s.send(pkt)
	}
	if flush {
		s.send(s.batch.Flush())
	}
}

// ---- WebSocket ----------------------------------------------------------------

func (s *Server) onChange(b *book.Book) {
	s.wsMu.Lock()
	for c := range s.clients {
		if c.symbol == b.Name {
			c.mu.Lock()
			c.dirty = true
			c.mu.Unlock()
		}
	}
	s.wsMu.Unlock()
}

func (s *Server) onTrade(b *book.Book, t book.Trade) {
	s.wsMu.Lock()
	for c := range s.clients {
		if c.symbol == b.Name {
			c.mu.Lock()
			if len(c.trades) < 200 {
				c.trades = append(c.trades, t)
			}
			c.dirty = true
			c.mu.Unlock()
		}
	}
	s.wsMu.Unlock()
}

type depthMsg struct {
	Type   string       `json:"type"`
	Symbol string       `json:"symbol"`
	Bids   []book.Level `json:"bids"`
	Asks   []book.Level `json:"asks"`
	Last   uint32       `json:"last"`
	Volume uint64       `json:"volume"`
	Trades []book.Trade `json:"trades"`
	TS     int64        `json:"ts"`
}

// SymbolInfo is one row of /api/symbols.
type SymbolInfo struct {
	Symbol string `json:"symbol"`
	Bid    uint32 `json:"bid"`
	Ask    uint32 `json:"ask"`
	Last   uint32 `json:"last"`
	Volume uint64 `json:"volume"`
	Trades uint64 `json:"trades"`
}

func (s *Server) Symbols() []SymbolInfo {
	s.mu.RLock()
	defer s.mu.RUnlock()
	var out []SymbolInfo
	for _, b := range s.books.All() {
		bid, _, ask, _ := b.Best()
		out = append(out, SymbolInfo{b.Name, bid, ask, b.Last, b.Volume, b.Trades})
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Symbol < out[j].Symbol })
	return out
}

func (s *Server) depth(symbol string, levels int) *depthMsg {
	s.mu.RLock()
	defer s.mu.RUnlock()
	b := s.books.Get(symbol)
	if b == nil {
		return nil
	}
	bids, asks := b.Depth(levels)
	return &depthMsg{Type: "book", Symbol: symbol, Bids: bids, Asks: asks, Last: b.Last, Volume: b.Volume, TS: NowOfDay()}
}

func (s *Server) serveWS(w http.ResponseWriter, r *http.Request) {
	conn, err := ws.Upgrade(w, r)
	if err != nil {
		return
	}
	c := &wsClient{conn: conn, symbol: r.URL.Query().Get("symbol"), dirty: true, wake: make(chan struct{}, 1)}
	s.wsMu.Lock()
	s.clients[c] = true
	s.wsMu.Unlock()
	defer func() {
		s.wsMu.Lock()
		delete(s.clients, c)
		s.wsMu.Unlock()
		conn.Close()
	}()
	symbols, _ := json.Marshal(map[string]any{"type": "symbols", "symbols": s.Symbols()})
	if conn.WriteText(symbols) != nil {
		return
	}
	go func() {
		for {
			p, err := conn.ReadMessage()
			if err != nil {
				conn.Close()
				return
			}
			var req struct {
				Subscribe string `json:"subscribe"`
			}
			if json.Unmarshal(p, &req) == nil && req.Subscribe != "" {
				c.mu.Lock()
				c.symbol, c.dirty, c.trades = req.Subscribe, true, nil
				c.mu.Unlock()
			}
		}
	}()
	// conflate: at most one book message every 50 ms per client
	tick := time.NewTicker(50 * time.Millisecond)
	defer tick.Stop()
	lastSymbols := time.Now()
	for range tick.C {
		c.mu.Lock()
		dirty, sym, trades := c.dirty, c.symbol, c.trades
		c.dirty, c.trades = false, nil
		c.mu.Unlock()
		if time.Since(lastSymbols) > time.Second {
			lastSymbols = time.Now()
			b, _ := json.Marshal(map[string]any{"type": "symbols", "symbols": s.Symbols()})
			if conn.WriteText(b) != nil {
				return
			}
		}
		if !dirty || sym == "" {
			continue
		}
		d := s.depth(sym, 15)
		if d == nil {
			continue
		}
		d.Trades = trades
		b, _ := json.Marshal(d)
		if conn.WriteText(b) != nil {
			return
		}
	}
}

// Handler serves the WebSocket at /ws, the JSON API under /api and static files.
func (s *Server) Handler(static string) http.Handler {
	mux := http.NewServeMux()
	mux.HandleFunc("/ws", s.serveWS)
	mux.HandleFunc("/api/symbols", func(w http.ResponseWriter, r *http.Request) {
		writeJSON(w, s.Symbols())
	})
	mux.HandleFunc("/api/book", func(w http.ResponseWriter, r *http.Request) {
		n, _ := strconv.Atoi(r.URL.Query().Get("levels"))
		if n <= 0 {
			n = 10
		}
		d := s.depth(r.URL.Query().Get("symbol"), n)
		if d == nil {
			http.Error(w, "unknown symbol", http.StatusNotFound)
			return
		}
		writeJSON(w, d)
	})
	mux.HandleFunc("/api/stats", func(w http.ResponseWriter, r *http.Request) {
		s.mu.RLock()
		st := s.books.Stats
		live := s.books.LiveOrders()
		s.mu.RUnlock()
		s.wsMu.Lock()
		nws := len(s.clients)
		s.wsMu.Unlock()
		writeJSON(w, map[string]any{
			"messages": s.Messages.Load(), "packets": s.Packets.Load(), "retransmitted": s.Retransmits.Load(),
			"udp_subscribers": s.Subscribers(), "websocket_clients": nws, "live_orders": live,
			"unknown_refs": st.UnknownRefs, "crossed": st.Crossed, "locked": st.Locked,
			"feed_latency_us": s.FeedLatency.Percentiles(),
		})
	})
	if static != "" {
		files := http.FileServer(http.Dir(static))
		mux.Handle("/", http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			w.Header().Set("Cache-Control", "no-cache")
			files.ServeHTTP(w, r)
		}))
	}
	return mux
}

func writeJSON(w http.ResponseWriter, v any) {
	w.Header().Set("Content-Type", "application/json")
	w.Header().Set("Access-Control-Allow-Origin", "*")
	if err := json.NewEncoder(w).Encode(v); err != nil {
		log.Println("md:", err)
	}
}
