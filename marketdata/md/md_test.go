package md

import (
	"bufio"
	"encoding/json"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"bourse/marketdata/book"
	"bourse/marketdata/itch"
	"bourse/marketdata/mold"
	"bourse/marketdata/ws"
)

func TestItchRoundTrip(t *testing.T) {
	var m itch.Msg
	if err := itch.Decode(itch.AddOrder(3, 123456789, 42, 'B', 300, "AAPL", 1543200), &m); err != nil {
		t.Fatal(err)
	}
	if m.Type != 'A' || m.Locate != 3 || m.Timestamp != 123456789 || m.Ref != 42 || m.Side != 'B' || m.Shares != 300 || m.Stock != "AAPL" || m.Price != 1543200 {
		t.Fatalf("add order: %+v", m)
	}
	itch.Decode(itch.Replace(3, 5, 42, 43, 200, 1543100), &m)
	if m.Type != 'U' || m.Ref != 42 || m.NewRef != 43 || m.Shares != 200 || m.Price != 1543100 {
		t.Fatalf("replace: %+v", m)
	}
	itch.Decode(itch.StockDirectory(3, 1, "MSFT", 100), &m)
	if m.Stock != "MSFT" || m.RoundLot != 100 {
		t.Fatalf("directory: %+v", m)
	}
	for _, b := range [][]byte{itch.Executed(1, 1, 1, 1, 1), itch.Cancel(1, 1, 1, 1), itch.Delete(1, 1, 1), itch.SystemEvent(1, 'Q')} {
		if itch.Length(b[0]) != len(b) {
			t.Fatalf("length of %c: %d, want %d", b[0], len(b), itch.Length(b[0]))
		}
	}
}

func apply(bs *book.Books, msgs ...[]byte) {
	var m itch.Msg
	for _, b := range msgs {
		itch.Decode(b, &m)
		bs.Apply(&m)
	}
}

func TestBook(t *testing.T) {
	bs := book.New()
	var trades []book.Trade
	bs.OnTrade = func(b *book.Book, tr book.Trade) { trades = append(trades, tr) }
	apply(bs,
		itch.StockDirectory(1, 0, "AAPL", 100),
		itch.SystemEvent(0, 'Q'),
		itch.AddOrder(1, 1, 1, 'B', 100, "AAPL", 1000000),
		itch.AddOrder(1, 2, 2, 'B', 200, "AAPL", 1000000),
		itch.AddOrder(1, 3, 3, 'B', 300, "AAPL", 990000),
		itch.AddOrder(1, 4, 4, 'S', 100, "AAPL", 1010000),
		itch.Executed(1, 5, 1, 40, 7),
		itch.Cancel(1, 6, 2, 50),
		itch.Replace(1, 7, 3, 5, 300, 1000000),
		itch.Delete(1, 8, 4),
	)
	b := bs.Get("AAPL")
	bids, asks := b.Depth(5)
	if len(asks) != 0 || len(bids) != 1 || bids[0].Price != 1000000 || bids[0].Shares != 60+150+300 || bids[0].Orders != 3 {
		t.Fatalf("depth: %+v %+v", bids, asks)
	}
	if len(trades) != 1 || trades[0].Shares != 40 || trades[0].Side != "S" || b.Volume != 40 {
		t.Fatalf("trades: %+v", trades)
	}
	if bs.Stats.UnknownRefs != 0 || bs.Stats.Crossed != 0 || bs.LiveOrders() != 3 {
		t.Fatalf("stats: %+v live %d", bs.Stats, bs.LiveOrders())
	}
	apply(bs, itch.Delete(1, 9, 99), itch.AddOrder(1, 10, 6, 'S', 10, "AAPL", 990000))
	if bs.Stats.UnknownRefs != 1 || bs.Stats.Crossed != 1 {
		t.Fatalf("a bad feed is noticed: %+v", bs.Stats)
	}
}

func TestMold(t *testing.T) {
	s := mold.Session("TEST")
	b := mold.NewBatcher(s)
	var pkts [][]byte
	big := make([]byte, 600)
	for i := uint64(1); i <= 5; i++ {
		if p := b.Add(i, big); p != nil {
			pkts = append(pkts, p)
		}
	}
	pkts = append(pkts, b.Flush())
	var seqs []uint64
	total := 0
	for _, p := range pkts {
		if len(p) > mold.MaxPayload {
			t.Fatalf("packet of %d bytes", len(p))
		}
		_, seq, msgs, err := mold.Parse(p)
		if err != nil {
			t.Fatal(err)
		}
		seqs = append(seqs, seq)
		total += len(msgs)
	}
	if total != 5 || seqs[0] != 1 || seqs[1] != 3 || seqs[2] != 5 {
		t.Fatalf("batches: %v total %d", seqs, total)
	}
	var st mold.Store
	for i := 0; i < 10; i++ {
		st.Append([]byte{byte(i)})
	}
	got := st.Get(4, 3)
	if len(got) != 3 || got[0][0] != 3 || st.Next() != 11 {
		t.Fatalf("store: %v", got)
	}
}

func TestWebSocketKey(t *testing.T) {
	// the example of RFC 6455, section 1.3
	if k := ws.AcceptKey("dGhlIHNhbXBsZSBub25jZQ=="); k != "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=" {
		t.Fatal(k)
	}
}

func feed(s *Server) {
	s.Publish(itch.SystemEvent(1, 'O'), false)
	s.Publish(itch.StockDirectory(1, 1, "AAPL", 100), false)
	s.Publish(itch.SystemEvent(1, 'Q'), true)
	for i := uint64(1); i <= 100; i++ {
		side := byte('B')
		price := uint32(1000000 - 100*i)
		if i%2 == 0 {
			side, price = 'S', uint32(1010000+100*i)
		}
		s.Publish(itch.AddOrder(1, uint64(NowOfDay()), i, side, 100, "AAPL", price), i%10 == 0)
	}
}

func TestUDPFanOutAndRetransmission(t *testing.T) {
	s := New("TEST")
	if err := s.ListenUDP("127.0.0.1:0"); err != nil {
		t.Fatal(err)
	}
	srv, _ := net.ResolveUDPAddr("udp", s.UDPAddr())
	c, _ := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	defer c.Close()
	c.WriteToUDP([]byte("JOIN"), srv)
	for i := 0; i < 100 && s.Subscribers() == 0; i++ {
		time.Sleep(10 * time.Millisecond)
	}
	feed(s)
	got := map[uint64]bool{}
	buf := make([]byte, 65536)
	c.SetReadDeadline(time.Now().Add(2 * time.Second))
	for len(got) < 103 {
		n, _, err := c.ReadFromUDP(buf)
		if err != nil {
			t.Fatalf("received %d of 103 messages: %v", len(got), err)
		}
		_, seq, msgs, _ := mold.Parse(buf[:n])
		for i := range msgs {
			got[seq+uint64(i)] = true
		}
	}
	// ask again for messages 10..14
	c.WriteToUDP(mold.Request(mold.Session("TEST"), 10, 5), srv)
	c.SetReadDeadline(time.Now().Add(2 * time.Second))
	for {
		n, _, err := c.ReadFromUDP(buf)
		if err != nil {
			t.Fatal("no retransmission")
		}
		_, seq, msgs, _ := mold.Parse(buf[:n])
		if seq == 10 && len(msgs) == 5 {
			break
		}
	}
	if s.Retransmits.Load() != 5 {
		t.Fatalf("retransmitted %d", s.Retransmits.Load())
	}
}

func TestHTTPAndWebSocket(t *testing.T) {
	s := New("TEST")
	feed(s)
	h := httptest.NewServer(s.Handler(""))
	defer h.Close()
	resp, err := http.Get(h.URL + "/api/book?symbol=AAPL&levels=3")
	if err != nil {
		t.Fatal(err)
	}
	var d struct {
		Bids []book.Level `json:"bids"`
		Asks []book.Level `json:"asks"`
	}
	json.NewDecoder(resp.Body).Decode(&d)
	if len(d.Bids) != 3 || d.Bids[0].Price != 1000000-100 || len(d.Asks) != 3 || d.Asks[0].Price != 1010000+200 {
		t.Fatalf("book: %+v", d)
	}
	// a WebSocket client subscribed to AAPL gets the symbol list, then the book
	conn, err := net.Dial("tcp", strings.TrimPrefix(h.URL, "http://"))
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	conn.Write([]byte("GET /ws?symbol=AAPL HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n"))
	r := bufio.NewReader(conn)
	line, _ := r.ReadString('\n')
	if !strings.Contains(line, "101") {
		t.Fatalf("handshake: %q", line)
	}
	for {
		l, _ := r.ReadString('\n')
		if l == "\r\n" {
			break
		}
	}
	types := map[string]bool{}
	conn.SetReadDeadline(time.Now().Add(2 * time.Second))
	for !types["book"] {
		h := make([]byte, 2)
		if _, err := r.Read(h[:1]); err != nil {
			t.Fatal(err)
		}
		r.Read(h[1:])
		n := int(h[1] & 0x7F)
		if n == 126 {
			e := make([]byte, 2)
			r.Read(e[:1])
			r.Read(e[1:])
			n = int(e[0])<<8 | int(e[1])
		}
		p := make([]byte, n)
		for got := 0; got < n; {
			k, err := r.Read(p[got:])
			if err != nil {
				t.Fatal(err)
			}
			got += k
		}
		var msg map[string]any
		if err := json.Unmarshal(p, &msg); err != nil {
			t.Fatalf("frame %q: %v", p, err)
		}
		types[msg["type"].(string)] = true
	}
	if !types["symbols"] {
		t.Fatal("no symbol list")
	}
}
