//go:build !windows

package md

import "time"

// WallNanos: nanoseconds since the Unix epoch.
func WallNanos() int64 { return time.Now().UnixNano() }
