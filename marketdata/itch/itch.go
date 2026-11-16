// Package itch decodes NASDAQ TotalView-ITCH 5.0 messages: the ones that
// change order books, plus the stock directory, trading actions and system
// events. Fields are big-endian; every message starts with type, stock
// locate, tracking number and a 6-byte timestamp (ns since midnight).
package itch

import (
	"bufio"
	"encoding/binary"
	"errors"
	"io"
	"strings"
)

// Length of each message type (0: unknown).
func Length(t byte) int {
	switch t {
	case 'S', 'W':
		return 12
	case 'R':
		return 39
	case 'H':
		return 25
	case 'Y', 'N':
		return 20
	case 'L':
		return 26
	case 'V', 'J', 'U':
		return 35
	case 'K':
		return 28
	case 'h':
		return 21
	case 'A':
		return 36
	case 'F', 'Q':
		return 40
	case 'E':
		return 31
	case 'C':
		return 36
	case 'X':
		return 23
	case 'D', 'B':
		return 19
	case 'P':
		return 44
	case 'I':
		return 50
	}
	return 0
}

// Msg is a decoded message; only the fields of its type are set.
type Msg struct {
	Type      byte
	Locate    uint16
	Timestamp uint64 // ns since midnight
	Ref       uint64 // order reference (A F E C X D P; U: the original)
	NewRef    uint64 // U
	Side      byte   // A F P: 'B' or 'S'
	Shares    uint32
	Stock     string // A F P R H
	Price     uint32 // A F U P C: 1/10000 dollar
	Match     uint64 // E C P
	Printable bool   // C
	Code      byte   // S: event code; H: trading state
	RoundLot  uint32 // R
}

var ErrShort = errors.New("itch: message shorter than its type requires")

func be48(b []byte) uint64 {
	return uint64(binary.BigEndian.Uint16(b))<<32 | uint64(binary.BigEndian.Uint32(b[2:]))
}

func stock(b []byte) string { return strings.TrimRight(string(b), " ") }

// Decode one message (without its length prefix).
func Decode(b []byte, m *Msg) error {
	if len(b) == 0 {
		return ErrShort
	}
	t := b[0]
	if n := Length(t); n == 0 || len(b) < n {
		if n == 0 {
			*m = Msg{Type: t}
			return nil
		}
		return ErrShort
	}
	*m = Msg{Type: t, Locate: binary.BigEndian.Uint16(b[1:]), Timestamp: be48(b[5:])}
	switch t {
	case 'S':
		m.Code = b[11]
	case 'R':
		m.Stock = stock(b[11:19])
		m.RoundLot = binary.BigEndian.Uint32(b[21:])
	case 'H':
		m.Stock = stock(b[11:19])
		m.Code = b[19]
	case 'A', 'F':
		m.Ref = binary.BigEndian.Uint64(b[11:])
		m.Side = b[19]
		m.Shares = binary.BigEndian.Uint32(b[20:])
		m.Stock = stock(b[24:32])
		m.Price = binary.BigEndian.Uint32(b[32:])
	case 'E':
		m.Ref = binary.BigEndian.Uint64(b[11:])
		m.Shares = binary.BigEndian.Uint32(b[19:])
		m.Match = binary.BigEndian.Uint64(b[23:])
	case 'C':
		m.Ref = binary.BigEndian.Uint64(b[11:])
		m.Shares = binary.BigEndian.Uint32(b[19:])
		m.Match = binary.BigEndian.Uint64(b[23:])
		m.Printable = b[31] == 'Y'
		m.Price = binary.BigEndian.Uint32(b[32:])
	case 'X':
		m.Ref = binary.BigEndian.Uint64(b[11:])
		m.Shares = binary.BigEndian.Uint32(b[19:])
	case 'D':
		m.Ref = binary.BigEndian.Uint64(b[11:])
	case 'U':
		m.Ref = binary.BigEndian.Uint64(b[11:])
		m.NewRef = binary.BigEndian.Uint64(b[19:])
		m.Shares = binary.BigEndian.Uint32(b[27:])
		m.Price = binary.BigEndian.Uint32(b[31:])
	case 'P':
		m.Ref = binary.BigEndian.Uint64(b[11:])
		m.Side = b[19]
		m.Shares = binary.BigEndian.Uint32(b[20:])
		m.Stock = stock(b[24:32])
		m.Price = binary.BigEndian.Uint32(b[32:])
		m.Match = binary.BigEndian.Uint64(b[36:])
	}
	return nil
}

// Reader reads a file of messages, each preceded by a big-endian u16 length.
type Reader struct {
	r   *bufio.Reader
	buf []byte
}

func NewReader(r io.Reader) *Reader {
	return &Reader{r: bufio.NewReaderSize(r, 1<<22), buf: make([]byte, 65536)}
}

// Next returns the next raw message (valid until the next call), or io.EOF.
func (rd *Reader) Next() ([]byte, error) {
	var hdr [2]byte
	if _, err := io.ReadFull(rd.r, hdr[:]); err != nil {
		if err == io.ErrUnexpectedEOF {
			return nil, io.EOF
		}
		return nil, err
	}
	n := int(binary.BigEndian.Uint16(hdr[:]))
	if _, err := io.ReadFull(rd.r, rd.buf[:n]); err != nil {
		return nil, io.EOF // a message cut off at the end of the file
	}
	return rd.buf[:n], nil
}

// ---- encoding (used by tests and the benchmark publisher) -----------------

func header(b []byte, t byte, locate uint16, ts uint64) {
	b[0] = t
	binary.BigEndian.PutUint16(b[1:], locate)
	binary.BigEndian.PutUint16(b[3:], 0)
	binary.BigEndian.PutUint16(b[5:], uint16(ts>>32))
	binary.BigEndian.PutUint32(b[7:], uint32(ts))
}

func pad(dst []byte, s string) {
	for i := range dst {
		if i < len(s) {
			dst[i] = s[i]
		} else {
			dst[i] = ' '
		}
	}
}

func AddOrder(locate uint16, ts, ref uint64, side byte, shares uint32, stock string, price uint32) []byte {
	b := make([]byte, 36)
	header(b, 'A', locate, ts)
	binary.BigEndian.PutUint64(b[11:], ref)
	b[19] = side
	binary.BigEndian.PutUint32(b[20:], shares)
	pad(b[24:32], stock)
	binary.BigEndian.PutUint32(b[32:], price)
	return b
}

func Executed(locate uint16, ts, ref uint64, shares uint32, match uint64) []byte {
	b := make([]byte, 31)
	header(b, 'E', locate, ts)
	binary.BigEndian.PutUint64(b[11:], ref)
	binary.BigEndian.PutUint32(b[19:], shares)
	binary.BigEndian.PutUint64(b[23:], match)
	return b
}

func Cancel(locate uint16, ts, ref uint64, shares uint32) []byte {
	b := make([]byte, 23)
	header(b, 'X', locate, ts)
	binary.BigEndian.PutUint64(b[11:], ref)
	binary.BigEndian.PutUint32(b[19:], shares)
	return b
}

func Delete(locate uint16, ts, ref uint64) []byte {
	b := make([]byte, 19)
	header(b, 'D', locate, ts)
	binary.BigEndian.PutUint64(b[11:], ref)
	return b
}

func Replace(locate uint16, ts, orig, nref uint64, shares, price uint32) []byte {
	b := make([]byte, 35)
	header(b, 'U', locate, ts)
	binary.BigEndian.PutUint64(b[11:], orig)
	binary.BigEndian.PutUint64(b[19:], nref)
	binary.BigEndian.PutUint32(b[27:], shares)
	binary.BigEndian.PutUint32(b[31:], price)
	return b
}

func StockDirectory(locate uint16, ts uint64, stock string, roundLot uint32) []byte {
	b := make([]byte, 39)
	header(b, 'R', locate, ts)
	pad(b[11:19], stock)
	b[19], b[20] = 'Q', 'N'
	binary.BigEndian.PutUint32(b[21:], roundLot)
	copy(b[25:], []byte{'N', 'C', ' ', ' ', 'P', 'N', ' ', '1', 'N', 0, 0, 0, 0, 'N'})
	return b
}

func SystemEvent(ts uint64, code byte) []byte {
	b := make([]byte, 12)
	header(b, 'S', 0, ts)
	b[11] = code
	return b
}
