// Package soup is a SoupBinTCP 4.0 client: it logs in to the ITCH gateway
// and delivers the sequenced messages.
package soup

import (
	"bufio"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"strings"
	"time"
)

type Client struct {
	conn net.Conn
	r    *bufio.Reader
	// Next is the sequence number of the next message to arrive
	Next uint64
}

func pad(s string, n int) string {
	if len(s) > n {
		return s[:n]
	}
	return s + strings.Repeat(" ", n-len(s))
}

// Dial connects and logs in, asking for messages from sequence `from` (1: the start).
func Dial(addr, user, password string, from uint64) (*Client, error) {
	c, err := net.DialTimeout("tcp", addr, 5*time.Second)
	if err != nil {
		return nil, err
	}
	if tc, ok := c.(*net.TCPConn); ok {
		tc.SetNoDelay(true)
	}
	payload := pad(user, 6) + pad(password, 10) + fmt.Sprintf("%10s", "") + fmt.Sprintf("%20d", from)
	pkt := make([]byte, 3+len(payload))
	binary.BigEndian.PutUint16(pkt, uint16(1+len(payload)))
	pkt[2] = 'L'
	copy(pkt[3:], payload)
	if _, err := c.Write(pkt); err != nil {
		c.Close()
		return nil, err
	}
	cl := &Client{conn: c, r: bufio.NewReaderSize(c, 1<<20)}
	t, p, err := cl.packet()
	if err != nil {
		c.Close()
		return nil, err
	}
	if t != 'A' || len(p) < 30 {
		c.Close()
		return nil, fmt.Errorf("soup: login refused (%q)", string(p))
	}
	fmt.Sscan(strings.TrimSpace(string(p[10:30])), &cl.Next)
	go cl.heartbeat()
	return cl, nil
}

func (c *Client) heartbeat() {
	pkt := []byte{0, 1, 'R'}
	for {
		time.Sleep(time.Second)
		if _, err := c.conn.Write(pkt); err != nil {
			return
		}
	}
}

func (c *Client) packet() (byte, []byte, error) {
	var hdr [2]byte
	if _, err := io.ReadFull(c.r, hdr[:]); err != nil {
		return 0, nil, err
	}
	n := int(binary.BigEndian.Uint16(hdr[:]))
	if n == 0 {
		return 0, nil, nil
	}
	b := make([]byte, n)
	if _, err := io.ReadFull(c.r, b); err != nil {
		return 0, nil, err
	}
	return b[0], b[1:], nil
}

// Read blocks until the next sequenced message (heartbeats are skipped).
// It returns the message and its sequence number.
func (c *Client) Read() ([]byte, uint64, error) {
	for {
		t, p, err := c.packet()
		if err != nil {
			return nil, 0, err
		}
		switch t {
		case 'S':
			seq := c.Next
			c.Next++
			return p, seq, nil
		case 'Z':
			return nil, 0, io.EOF
		}
	}
}

// Buffered reports how many bytes are already read from the socket and not
// yet delivered (0 means the next Read may block).
func (c *Client) Buffered() int { return c.r.Buffered() }

func (c *Client) Close() error { return c.conn.Close() }
