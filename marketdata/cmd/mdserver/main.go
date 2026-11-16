// mdserver: the market-data distributor of Bourse.
//
//	mdserver -itch 127.0.0.1:9003 -udp 127.0.0.1:9010 -http 127.0.0.1:8080 -static ../ui
//
// It logs in to the ITCH gateway (SoupBinTCP), rebuilds the books, and
// serves MoldUDP64 subscribers, WebSocket clients and a JSON API.
package main

import (
	"flag"
	"log"
	"net/http"
	"time"

	"bourse/marketdata/md"
	"bourse/marketdata/soup"
)

func main() {
	itchAddr := flag.String("itch", "127.0.0.1:9003", "ITCH gateway (SoupBinTCP)")
	udpAddr := flag.String("udp", "127.0.0.1:9010", "MoldUDP64 publisher and retransmission address")
	httpAddr := flag.String("http", "127.0.0.1:8080", "HTTP address (WebSocket, API, static files)")
	static := flag.String("static", "", "directory of the web client")
	session := flag.String("session", "BOURSE", "MoldUDP64 session name")
	flag.Parse()

	s := md.New(*session)
	if err := s.ListenUDP(*udpAddr); err != nil {
		log.Fatal(err)
	}
	go func() {
		log.Fatal(http.ListenAndServe(*httpAddr, s.Handler(*static)))
	}()
	log.Printf("mdserver: udp %s, http %s", s.UDPAddr(), *httpAddr)
	var next uint64 = 1
	for {
		c, err := soup.Dial(*itchAddr, "MD", "", next)
		if err != nil {
			log.Printf("mdserver: %v; retrying", err)
			time.Sleep(time.Second)
			continue
		}
		log.Printf("mdserver: connected to %s from message %d", *itchAddr, next)
		for {
			m, seq, err := c.Read()
			if err != nil {
				log.Printf("mdserver: feed: %v", err)
				break
			}
			if seq != next {
				continue // already have it (a reconnect replays from where we asked)
			}
			next++
			s.Publish(m, c.Buffered() == 0)
		}
		c.Close()
		time.Sleep(500 * time.Millisecond)
	}
}
