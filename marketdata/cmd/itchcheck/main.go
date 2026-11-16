// itchcheck: replay an ITCH 5.0 file through the Go books and print the
// same consistency counters as the C++ itch-replay, to compare the two.
//
//	itchcheck FILE [-max N]
package main

import (
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"os"
	"time"

	"bourse/marketdata/book"
	"bourse/marketdata/itch"
)

func main() {
	max := flag.Uint64("max", 0, "stop after this many messages (0: all)")
	flag.Parse()
	if flag.NArg() != 1 {
		fmt.Fprintln(os.Stderr, "usage: itchcheck FILE [-max N]")
		os.Exit(2)
	}
	f, err := os.Open(flag.Arg(0))
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	defer f.Close()
	rd := itch.NewReader(f)
	bs := book.New()
	var m itch.Msg
	t0 := time.Now()
	for *max == 0 || bs.Stats.Messages < *max {
		raw, err := rd.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		if itch.Decode(raw, &m) != nil {
			continue
		}
		bs.Apply(&m)
	}
	secs := time.Since(t0).Seconds()
	out := map[string]any{
		"messages":                   bs.Stats.Messages,
		"seconds":                    secs,
		"messages_per_second":        float64(bs.Stats.Messages) / secs,
		"orders_added":               bs.Stats.OrdersAdded,
		"live_orders_at_end":         bs.LiveOrders(),
		"unknown_order_references":   bs.Stats.UnknownRefs,
		"executions_or_cancels_over": bs.Stats.OverExecuted,
		"book_checks":                bs.Stats.CrossedChecks,
		"crossed":                    bs.Stats.Crossed,
		"locked":                     bs.Stats.Locked,
		"executed_shares":            bs.Stats.ExecutedShares,
		"hidden_trade_shares":        bs.Stats.HiddenShares,
	}
	b, _ := json.MarshalIndent(out, "", " ")
	fmt.Println(string(b))
}
