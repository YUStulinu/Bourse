// mdbench: fan-out of the market-data server to many MoldUDP64 subscribers.
//
//	mdbench -udp 127.0.0.1:9010 -subscribers 50 -seconds 20 [-drop 0.001] [-json OUT]
//
// Each subscriber joins, receives packets, checks that sequence numbers are
// contiguous, asks for retransmission of what it missed (and of packets it
// drops on purpose with -drop, to test recovery), and measures the latency
// of each message: arrival time minus the ITCH timestamp the engine gave it
// (same machine, same clock).
package main

import (
	"encoding/binary"
	"encoding/json"
	"flag"
	"fmt"
	"math/rand"
	"net"
	"os"
	"sort"
	"sync"
	"time"

	"bourse/marketdata/md"
	"bourse/marketdata/mold"
)

type result struct {
	received, gaps, recovered, duplicates uint64
	lat                                   []int64
	missing                               uint64
}

func subscriber(server *net.UDPAddr, seconds float64, drop float64, seed int64, out *result, wg *sync.WaitGroup) {
	defer wg.Done()
	c, err := net.ListenUDP("udp", &net.UDPAddr{IP: net.IPv4(127, 0, 0, 1)})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		return
	}
	defer c.Close()
	c.SetReadBuffer(8 << 20)
	c.WriteToUDP([]byte("JOIN"), server)
	rng := rand.New(rand.NewSource(seed))
	var next uint64 // next expected sequence (0: not started)
	have := map[uint64]bool{}
	pending := map[uint64]time.Time{} // missing sequence numbers we asked for
	buf := make([]byte, 65536)
	deadline := time.Now().Add(time.Duration(seconds * float64(time.Second)))
	var session [10]byte
	for time.Now().Before(deadline) {
		c.SetReadDeadline(time.Now().Add(200 * time.Millisecond))
		n, _, err := c.ReadFromUDP(buf)
		if err != nil {
			continue
		}
		now := md.NowOfDay()
		sess, seq, msgs, err := mold.Parse(buf[:n])
		if err != nil {
			continue
		}
		session = sess
		if len(msgs) > 0 && drop > 0 && rng.Float64() < drop && pending[seq].IsZero() && next != 0 && seq >= next {
			continue // simulate a lost packet
		}
		if next == 0 {
			next = seq
		}
		for i, m := range msgs {
			s := seq + uint64(i)
			if s < next && !pending[s].IsZero() {
				delete(pending, s)
				out.recovered++
				continue
			}
			if s < next || have[s] {
				out.duplicates++
				continue
			}
			if s > next {
				out.gaps++
				for g := next; g < s; g++ {
					pending[g] = time.Now()
				}
				c.WriteToUDP(mold.Request(session, next, uint16(s-next)), server)
			}
			next = s + 1
			out.received++
			if len(m) >= 11 && (m[0] == 'A' || m[0] == 'E' || m[0] == 'X' || m[0] == 'D' || m[0] == 'U') {
				ts := int64(binary.BigEndian.Uint16(m[5:]))<<32 | int64(binary.BigEndian.Uint32(m[7:]))
				out.lat = append(out.lat, (now-ts+86_400_000_000_000)%86_400_000_000_000)
			}
		}
		// a heartbeat carries the next sequence number: a gap at the end of a burst
		if len(msgs) == 0 && next != 0 && seq > next {
			for g := next; g < seq; g++ {
				pending[g] = time.Now()
			}
			c.WriteToUDP(mold.Request(session, next, uint16(seq-next)), server)
			out.gaps++
			next = seq
		}
		// ask again for what has not come back within 100 ms
		for s, t := range pending {
			if time.Since(t) > 100*time.Millisecond {
				c.WriteToUDP(mold.Request(session, s, 1), server)
				pending[s] = time.Now()
			}
		}
	}
	out.missing = uint64(len(pending))
	c.WriteToUDP([]byte("LEAVE"), server)
}

func main() {
	addr := flag.String("udp", "127.0.0.1:9010", "mdserver UDP address")
	n := flag.Int("subscribers", 10, "number of subscribers")
	seconds := flag.Float64("seconds", 10, "how long to listen")
	drop := flag.Float64("drop", 0, "fraction of packets each subscriber drops on purpose")
	jsonOut := flag.String("json", "", "append the result to this JSON file")
	flag.Parse()
	server, err := net.ResolveUDPAddr("udp", *addr)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	rs := make([]result, *n)
	var wg sync.WaitGroup
	for i := 0; i < *n; i++ {
		wg.Add(1)
		go subscriber(server, *seconds, *drop, int64(i+1), &rs[i], &wg)
	}
	wg.Wait()
	var all []int64
	var tot result
	for _, r := range rs {
		tot.received += r.received
		tot.gaps += r.gaps
		tot.recovered += r.recovered
		tot.duplicates += r.duplicates
		tot.missing += r.missing
		all = append(all, r.lat...)
	}
	sort.Slice(all, func(i, j int) bool { return all[i] < all[j] })
	pct := func(q float64) float64 {
		if len(all) == 0 {
			return 0
		}
		i := int(q * float64(len(all)))
		if i >= len(all) {
			i = len(all) - 1
		}
		return float64(all[i]) / 1000
	}
	res := map[string]any{
		"subscribers": *n, "seconds": *seconds, "drop": *drop,
		"messages_received": tot.received, "messages_per_second_all_subscribers": float64(tot.received) / *seconds,
		"gaps": tot.gaps, "recovered": tot.recovered, "still_missing": tot.missing, "duplicates": tot.duplicates,
		"latency_us": map[string]float64{"p50": pct(0.5), "p99": pct(0.99), "p99_9": pct(0.999), "max": pct(1)},
	}
	b, _ := json.Marshal(res)
	fmt.Println(string(b))
	if *jsonOut != "" {
		var prev []any
		if old, err := os.ReadFile(*jsonOut); err == nil {
			json.Unmarshal(old, &prev)
		}
		prev = append(prev, res)
		b, _ := json.MarshalIndent(prev, "", " ")
		os.WriteFile(*jsonOut, b, 0o644)
	}
}
