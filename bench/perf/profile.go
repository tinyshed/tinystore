package main

import (
	"flag"
	"log"
	"os"
	"runtime"
	"runtime/pprof"
)

var (
	cpuProfile   = flag.String("cpu-profile", "", "write a CPU profile")
	mutexProfile = flag.String("mutex-profile", "", "write a mutex contention profile")
)

func startProfiles() func() {
	var cpu *os.File
	if *cpuProfile != "" {
		var err error
		cpu, err = os.Create(*cpuProfile)
		if err != nil {
			log.Fatal(err)
		}
		if err = pprof.StartCPUProfile(cpu); err != nil {
			log.Fatal(err)
		}
	}
	if *mutexProfile != "" {
		runtime.SetMutexProfileFraction(1)
	}
	return func() {
		if cpu != nil {
			pprof.StopCPUProfile()
			if err := cpu.Close(); err != nil {
				log.Fatal(err)
			}
		}
		if *mutexProfile != "" {
			file, err := os.Create(*mutexProfile)
			if err != nil {
				log.Fatal(err)
			}
			if err = pprof.Lookup("mutex").WriteTo(file, 0); err != nil {
				log.Fatal(err)
			}
			if err = file.Close(); err != nil {
				log.Fatal(err)
			}
		}
	}
}
