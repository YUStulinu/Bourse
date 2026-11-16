// Package mold implements MoldUDP64: messages are sent in UDP packets that
// carry a session name, the sequence number of their first message and a
// count; a receiver that sees a gap asks the retransmission server for the
// missing messages with a packet of the same header (count = how many).
package mold

import (
	"encoding/binary"
	"errors"
	"strings"
	"sync"
)

const HeaderLen = 20 // session (10), sequence (8), count (2)

// MaxPayload keeps packets under a typical Ethernet MTU.
const MaxPayload = 1400

func Session(s string) [10]byte {
	var b [10]byte
	copy(b[:], s+strings.Repeat(" ", 10))
	return b
}

// Packet builds a packet with messages starting at sequence seq.
func Packet(session [10]byte, seq uint64, msgs [][]byte) []byte {
	n := HeaderLen
	for _, m := range msgs {
		n += 2 + len(m)
	}
	b := make([]byte, n)
	copy(b, session[:])
	binary.BigEndian.PutUint64(b[10:], seq)
	binary.BigEndian.PutUint16(b[18:], uint16(len(msgs)))
	o := HeaderLen
	for _, m := range msgs {
		binary.BigEndian.PutUint16(b[o:], uint16(len(m)))
		copy(b[o+2:], m)
		o += 2 + len(m)
	}
	return b
}

// Heartbeat: a packet with no messages carrying the next sequence number.
func Heartbeat(session [10]byte, next uint64) []byte { return Packet(session, next, nil) }

var ErrPacket = errors.New("mold: malformed packet")

// Parse a packet: its first sequence number and its messages.
func Parse(b []byte) (session [10]byte, seq uint64, msgs [][]byte, err error) {
	if len(b) < HeaderLen {
		return session, 0, nil, ErrPacket
	}
	copy(session[:], b)
	seq = binary.BigEndian.Uint64(b[10:])
	count := int(binary.BigEndian.Uint16(b[18:]))
	if count == 0xFFFF { // end of session
		return session, seq, nil, nil
	}
	o := HeaderLen
	for i := 0; i < count; i++ {
		if o+2 > len(b) {
			return session, seq, nil, ErrPacket
		}
		n := int(binary.BigEndian.Uint16(b[o:]))
		if o+2+n > len(b) {
			return session, seq, nil, ErrPacket
		}
		msgs = append(msgs, b[o+2:o+2+n])
		o += 2 + n
	}
	return session, seq, msgs, nil
}

// Request asks for count messages from seq.
func Request(session [10]byte, seq uint64, count uint16) []byte {
	b := make([]byte, HeaderLen)
	copy(b, session[:])
	binary.BigEndian.PutUint64(b[10:], seq)
	binary.BigEndian.PutUint16(b[18:], count)
	return b
}

// Store keeps every message sent, for retransmission (sequence numbers from 1).
type Store struct {
	mu   sync.RWMutex
	msgs [][]byte
}

func (s *Store) Append(m []byte) uint64 {
	s.mu.Lock()
	s.msgs = append(s.msgs, m)
	n := uint64(len(s.msgs))
	s.mu.Unlock()
	return n
}

func (s *Store) Next() uint64 {
	s.mu.RLock()
	defer s.mu.RUnlock()
	return uint64(len(s.msgs)) + 1
}

// Get up to count messages from seq, limited to fit one packet.
func (s *Store) Get(seq uint64, count int) [][]byte {
	s.mu.RLock()
	defer s.mu.RUnlock()
	var out [][]byte
	size := HeaderLen
	for i := seq; i < seq+uint64(count) && i >= 1 && i <= uint64(len(s.msgs)); i++ {
		m := s.msgs[i-1]
		if size+2+len(m) > MaxPayload && len(out) > 0 {
			break
		}
		size += 2 + len(m)
		out = append(out, m)
	}
	return out
}

// Batcher packs consecutive messages into packets.
type Batcher struct {
	session [10]byte
	first   uint64
	msgs    [][]byte
	size    int
}

func NewBatcher(session [10]byte) *Batcher { return &Batcher{session: session} }

// Add a message with its sequence number; returns a full packet to send, if any.
func (b *Batcher) Add(seq uint64, m []byte) []byte {
	var out []byte
	if len(b.msgs) > 0 && (b.size+2+len(m) > MaxPayload || seq != b.first+uint64(len(b.msgs))) {
		out = b.Flush()
	}
	if len(b.msgs) == 0 {
		b.first = seq
		b.size = HeaderLen
	}
	b.msgs = append(b.msgs, m)
	b.size += 2 + len(m)
	return out
}

// Flush the pending messages as one packet (nil if none).
func (b *Batcher) Flush() []byte {
	if len(b.msgs) == 0 {
		return nil
	}
	p := Packet(b.session, b.first, b.msgs)
	b.msgs = b.msgs[:0]
	return p
}
