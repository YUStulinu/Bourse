// Package ws is a small WebSocket (RFC 6455) server side: the handshake,
// text frames out, and reading the client's (masked) frames.
package ws

import (
	"bufio"
	"crypto/sha1"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"io"
	"net"
	"net/http"
	"strings"
	"sync"
	"time"
)

const guid = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

// AcceptKey computes Sec-WebSocket-Accept for a client key.
func AcceptKey(key string) string {
	h := sha1.Sum([]byte(key + guid))
	return base64.StdEncoding.EncodeToString(h[:])
}

type Conn struct {
	c  net.Conn
	rw *bufio.ReadWriter
	mu sync.Mutex
}

// Upgrade an HTTP request to a WebSocket connection.
func Upgrade(w http.ResponseWriter, r *http.Request) (*Conn, error) {
	if !strings.EqualFold(r.Header.Get("Upgrade"), "websocket") || r.Header.Get("Sec-WebSocket-Key") == "" {
		http.Error(w, "websocket only", http.StatusBadRequest)
		return nil, errors.New("ws: not a websocket request")
	}
	hj, ok := w.(http.Hijacker)
	if !ok {
		return nil, errors.New("ws: cannot hijack")
	}
	c, rw, err := hj.Hijack()
	if err != nil {
		return nil, err
	}
	resp := "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: " +
		AcceptKey(r.Header.Get("Sec-WebSocket-Key")) + "\r\n\r\n"
	if _, err := rw.WriteString(resp); err != nil {
		c.Close()
		return nil, err
	}
	if err := rw.Flush(); err != nil {
		c.Close()
		return nil, err
	}
	return &Conn{c: c, rw: rw}, nil
}

// Frame encodes one unmasked server frame.
func Frame(op byte, payload []byte) []byte {
	n := len(payload)
	var h []byte
	switch {
	case n < 126:
		h = []byte{0x80 | op, byte(n)}
	case n < 65536:
		h = []byte{0x80 | op, 126, 0, 0}
		binary.BigEndian.PutUint16(h[2:], uint16(n))
	default:
		h = make([]byte, 10)
		h[0], h[1] = 0x80|op, 127
		binary.BigEndian.PutUint64(h[2:], uint64(n))
	}
	return append(h, payload...)
}

// WriteText sends a text frame (safe for concurrent use).
func (c *Conn) WriteText(b []byte) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.c.SetWriteDeadline(time.Now().Add(5 * time.Second))
	if _, err := c.rw.Write(Frame(1, b)); err != nil {
		return err
	}
	return c.rw.Flush()
}

// ReadMessage returns the next text or binary message; pings are answered,
// a close frame ends with io.EOF.
func (c *Conn) ReadMessage() ([]byte, error) {
	for {
		var h [2]byte
		if _, err := io.ReadFull(c.rw, h[:]); err != nil {
			return nil, err
		}
		op := h[0] & 0x0F
		masked := h[1]&0x80 != 0
		n := uint64(h[1] & 0x7F)
		switch n {
		case 126:
			var e [2]byte
			if _, err := io.ReadFull(c.rw, e[:]); err != nil {
				return nil, err
			}
			n = uint64(binary.BigEndian.Uint16(e[:]))
		case 127:
			var e [8]byte
			if _, err := io.ReadFull(c.rw, e[:]); err != nil {
				return nil, err
			}
			n = binary.BigEndian.Uint64(e[:])
		}
		if n > 1<<20 {
			return nil, errors.New("ws: frame too large")
		}
		var mask [4]byte
		if masked {
			if _, err := io.ReadFull(c.rw, mask[:]); err != nil {
				return nil, err
			}
		}
		p := make([]byte, n)
		if _, err := io.ReadFull(c.rw, p); err != nil {
			return nil, err
		}
		if masked {
			for i := range p {
				p[i] ^= mask[i%4]
			}
		}
		switch op {
		case 8:
			return nil, io.EOF
		case 9:
			c.mu.Lock()
			c.rw.Write(Frame(10, p))
			c.rw.Flush()
			c.mu.Unlock()
		case 1, 2:
			return p, nil
		}
	}
}

func (c *Conn) Close() error { return c.c.Close() }
