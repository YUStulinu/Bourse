//go:build windows

package md

import (
	"syscall"
	"unsafe"
)

var preciseTime = syscall.NewLazyDLL("kernel32.dll").NewProc("GetSystemTimePreciseAsFileTime")

// WallNanos: nanoseconds since the Unix epoch from the precise system clock.
// (time.Now's wall clock on Windows moves in steps of about half a
// millisecond; the engine and the gateways read the precise one.)
func WallNanos() int64 {
	var ft syscall.Filetime
	preciseTime.Call(uintptr(unsafe.Pointer(&ft)))
	return ft.Nanoseconds()
}
