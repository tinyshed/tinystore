//! The machine among the store's own metrics, docs/guides/metrics/self-metrics.md: its
//! process's memory and processor, the system's memory, swap, processors by
//! core and, off Windows, its load, read through sysinfo as the instruments'
//! totals are written.
//!
//! A processor's use is the time it was busy since the read before, so the
//! first read gives none, and its gauges begin at the second, 15 seconds on.

use std::sync::{Mutex, PoisonError};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use super::Metrics;
use super::instruments::Gauge;
use crate::Result;

pub(super) struct Machine {
    system: Mutex<Read>,
    pid: Option<Pid>,
    process_memory: Gauge,
    process_virtual_memory: Gauge,
    process_cpu: Gauge,
    memory: Gauge,
    memory_available: Gauge,
    swap: Gauge,
    swap_used: Gauge,
    cpus: Gauge,
    cpu: Gauge,
    core_cpu: Gauge,
    #[cfg(not(windows))]
    load: Gauge,
}

struct Read {
    system: System,
    /// Whether a read came before, which a processor's use is counted from.
    once: bool,
}

impl Machine {
    pub(super) fn open(metrics: &Metrics) -> Result<Self> {
        let gauge = |name: &str, unit: &str, help: &str| metrics.gauge(name).own().unit(unit).help(help).open();
        Ok(Self {
            system: Mutex::new(Read { system: System::new(), once: false }),
            pid: sysinfo::get_current_pid().ok(),
            process_memory: gauge("tinystore_process_memory_bytes", "bytes", "The process's resident memory.")?,
            process_virtual_memory: gauge(
                "tinystore_process_virtual_memory_bytes",
                "bytes",
                "The process's virtual memory.",
            )?,
            process_cpu: gauge("tinystore_process_cpu_percent", "%", "The process's processor use, 100 a core.")?,
            memory: gauge("tinystore_system_memory_bytes", "bytes", "The system's memory.")?,
            memory_available: gauge("tinystore_system_memory_available_bytes", "bytes", "Its memory available.")?,
            swap: gauge("tinystore_system_swap_bytes", "bytes", "The system's swap.")?,
            swap_used: gauge("tinystore_system_swap_used_bytes", "bytes", "Its swap in use.")?,
            cpus: gauge("tinystore_system_cpus", "cores", "The system's logical processors.")?,
            cpu: gauge("tinystore_system_cpu_percent", "%", "The system's processor use, 100 every core.")?,
            core_cpu: gauge("tinystore_system_core_cpu_percent", "%", "A logical processor's use.")?,
            #[cfg(not(windows))]
            load: gauge("tinystore_system_load", "processes", "The system's load average, by its span.")?,
        })
    }

    /// Reads the machine and sets its gauges.
    pub(super) fn read(&self) {
        let mut read = self.system.lock().unwrap_or_else(PoisonError::into_inner);
        let Read { system, once } = &mut *read;
        system.refresh_memory();
        system.refresh_cpu_usage();
        if let Some(pid) = self.pid {
            let only = ProcessRefreshKind::nothing().with_cpu().with_memory();
            system.refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, only);
        }
        self.memory.set(system.total_memory() as f64);
        self.memory_available.set(system.available_memory() as f64);
        self.swap.set(system.total_swap() as f64);
        self.swap_used.set(system.used_swap() as f64);
        self.cpus.set(system.cpus().len() as f64);
        let process = self.pid.and_then(|pid| system.process(pid));
        if let Some(process) = process {
            self.process_memory.set(process.memory() as f64);
            self.process_virtual_memory.set(process.virtual_memory() as f64);
        }
        if *once {
            self.cpu.set(f64::from(system.global_cpu_usage()));
            for (core, cpu) in system.cpus().iter().enumerate() {
                self.core_cpu.with(&[("core", &core.to_string())]).set(f64::from(cpu.cpu_usage()));
            }
            if let Some(process) = process {
                self.process_cpu.set(f64::from(process.cpu_usage()));
            }
        }
        *once = true;
        #[cfg(not(windows))]
        {
            let load = System::load_average();
            for (span, value) in [("1m", load.one), ("5m", load.five), ("15m", load.fifteen)] {
                self.load.with(&[("span", span)]).set(value);
            }
        }
    }
}
