// Copyright © 2022 Intel Corporation
//
// SPDX-License-Identifier: Apache-2.0
//

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use std::{fs, io, thread};

use log::warn;
use serde::Serialize;

#[derive(Debug)]
struct Tracer {
    events: HashMap<String, Vec<TraceEvent>>,
    start: Instant,
}

impl Tracer {
    fn new() -> Self {
        Self {
            events: HashMap::default(),
            start: Instant::now(),
        }
    }

    fn end(&self) {
        let end = Instant::now();
        // SAFETY: FFI call
        let path = format!("cloud-hypervisor-{}.trace", unsafe { libc::getpid() });

        #[derive(Serialize)]
        struct TraceReport<'a> {
            duration: Duration,
            events: &'a HashMap<String, Vec<TraceEvent>>,
        }

        let trace_report = TraceReport {
            duration: end.duration_since(self.start),
            events: &self.events,
        };

        let result = serde_json::to_vec_pretty(&trace_report)
            .map_err(io::Error::from)
            .and_then(|trace| fs::write(&path, trace));

        match result {
            Ok(()) => warn!("Trace output: {path}"),
            Err(e) => warn!("Failed to write trace output {path}: {e}"),
        }
    }
}

// Events outside start() and end() are dropped.
static TRACER: Mutex<Option<Tracer>> = Mutex::new(None);

thread_local! {
    static THREAD_DEPTH: Cell<u64> = const { Cell::new(0) };
}

#[derive(Clone, Debug, Serialize)]
struct TraceEvent {
    timestamp: Duration,
    event: &'static str,
    end_timestamp: Option<Duration>,
    depth: u64,
}

fn add_event(event: &'static str, start: Instant, end: Option<Instant>) {
    let depth = THREAD_DEPTH.get();
    let mut tracer = TRACER.lock().unwrap();
    let Some(tracer) = tracer.as_mut() else {
        return;
    };

    let trace_event = TraceEvent {
        timestamp: start.duration_since(tracer.start),
        event,
        end_timestamp: end.map(|end| end.duration_since(tracer.start)),
        depth,
    };

    let thread_name = thread::current().name().unwrap_or("").to_string();
    tracer
        .events
        .entry(thread_name)
        .or_default()
        .push(trace_event);
}

pub fn trace_point_log(event: &'static str) {
    add_event(event, Instant::now(), None);
}

pub struct TraceBlock {
    start: Instant,
    event: &'static str,
}

impl TraceBlock {
    pub fn new(event: &'static str) -> Self {
        THREAD_DEPTH.set(THREAD_DEPTH.get() + 1);
        Self {
            start: Instant::now(),
            event,
        }
    }
}

impl Drop for TraceBlock {
    fn drop(&mut self) {
        let end = Instant::now();
        THREAD_DEPTH.set(THREAD_DEPTH.get() - 1);
        add_event(self.event, self.start, Some(end));
    }
}

#[macro_export]
macro_rules! trace_point {
    ($event:expr) => {
        $crate::trace_point_log($event)
    };
}

#[macro_export]
macro_rules! trace_scoped {
    ($event:expr) => {
        let _trace_scoped = $crate::TraceBlock::new($event);
    };
}

pub fn end() {
    let tracer = TRACER.lock().unwrap().take();
    if let Some(tracer) = tracer {
        tracer.end();
    }
}

pub fn start() {
    *TRACER.lock().unwrap() = Some(Tracer::new());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_events() {
        // Neither an event outside a trace nor a second start() may panic.
        trace_point!("before_start");

        start();
        start();

        {
            trace_scoped!("outer");

            {
                trace_scoped!("inner");
            }

            trace_point!("point");
        }

        thread::Builder::new()
            .name("other".to_string())
            .spawn(|| {
                trace_scoped!("other");
            })
            .unwrap()
            .join()
            .unwrap();

        // Take the trace instead of calling end() so that no file is written.
        let tracer = TRACER.lock().unwrap().take().unwrap();
        trace_point!("after_end");

        let depths = |thread_name: &str| -> Vec<(&'static str, u64)> {
            tracer.events[thread_name]
                .iter()
                .map(|e| (e.event, e.depth))
                .collect()
        };

        let thread_name = thread::current().name().unwrap_or("").to_string();
        assert_eq!(
            depths(thread_name.as_str()),
            [("inner", 1), ("point", 1), ("outer", 0)]
        );
        assert_eq!(depths("other"), [("other", 0)]);
    }
}
