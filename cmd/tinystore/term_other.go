//go:build !windows

package main

import "os"

func enableColors(*os.File) bool { return true }
