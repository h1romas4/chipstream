//! Centralized logger abstraction for `soundlog-debuger`.
//!
//! Goals:
//! - Provide an output abstraction so code no longer calls `println!` / `eprintln!` directly.
//! - Support `dry-run`/no-op mode where formatting is avoided entirely.
//! - Handle broken pipe (e.g. `| head -n 10`) gracefully without panicking.
//!
//! Usage examples:
//! - Create a stdout logger: `let logger = Logger::new_stdout(false);`
//! - Create a no-op logger (dry-run): `let logger = Logger::new_stdout(true);`
//! - Create a logger backed by a custom writer (for tests): `Logger::with_writer(Box::new(writer))`
//!
//! The API favors passing `fmt::Arguments` (via `format_args!`) so that formatting work
//! is deferred until `write_fmt` is actually invoked. When the logger is Noop, `log`
//! returns immediately without calling `write_fmt`, avoiding allocation/format work.

#![allow(dead_code)]

use std::fmt;
use std::io::{self, ErrorKind, Write};
use std::sync::{Arc, Mutex};

/// Where the logger writes to.
pub enum LoggerOutput {
    /// Write to stdout (locked).
    Stdout,
    /// Write to stderr (locked).
    Stderr,
    /// No-op: do nothing and avoid formatting.
    Noop,
    /// Custom writer (protected by a mutex so Logger can be shared immutably).
    Writer(Arc<Mutex<Box<dyn Write + Send>>>),
}

/// Implement Clone so the output can be cheaply cloned (Arc clone for Writer).
impl Clone for LoggerOutput {
    fn clone(&self) -> Self {
        match self {
            LoggerOutput::Stdout => LoggerOutput::Stdout,
            LoggerOutput::Stderr => LoggerOutput::Stderr,
            LoggerOutput::Noop => LoggerOutput::Noop,
            LoggerOutput::Writer(mw) => LoggerOutput::Writer(Arc::clone(mw)),
        }
    }
}

/// Log levels for future expansion / filtering.
#[derive(Clone, Copy, Debug)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Debug,
}

/// Logger abstraction.
///
/// `exit_on_broken_pipe` controls whether the process should call `std::process::exit(0)`
/// when encountering a `BrokenPipe` IO error. This is useful for CLI semantics where
/// a program writing to a closed pipe should exit quietly. For unit tests, you can
/// set this to `false` to avoid terminating the test runner.
pub struct Logger {
    output: LoggerOutput,
    exit_on_broken_pipe: bool,
}

/// Allow cloning a Logger (cheap clone of underlying output).
impl Clone for Logger {
    fn clone(&self) -> Self {
        Self {
            output: self.output.clone(),
            exit_on_broken_pipe: self.exit_on_broken_pipe,
        }
    }
}

impl Logger {
    /// Create a stdout logger. If `dry_run` is true, the logger is a Noop.
    pub fn new_stdout(dry_run: bool) -> Self {
        if dry_run {
            Self {
                output: LoggerOutput::Noop,
                exit_on_broken_pipe: true,
            }
        } else {
            Self {
                output: LoggerOutput::Stdout,
                exit_on_broken_pipe: true,
            }
        }
    }

    /// Create a stderr logger. If `dry_run` is true, the logger is a Noop.
    pub fn new_stderr(dry_run: bool) -> Self {
        if dry_run {
            Self {
                output: LoggerOutput::Noop,
                exit_on_broken_pipe: true,
            }
        } else {
            Self {
                output: LoggerOutput::Stderr,
                exit_on_broken_pipe: true,
            }
        }
    }

    /// Create a Noop logger explicitly.
    pub fn new_noop() -> Self {
        Self {
            output: LoggerOutput::Noop,
            exit_on_broken_pipe: true,
        }
    }

    /// Create a logger backed by a custom writer.
    /// The writer is boxed and placed behind a mutex so the Logger can be shared.
    pub fn with_writer(writer: Box<dyn Write + Send>) -> Self {
        Self {
            output: LoggerOutput::Writer(Arc::new(Mutex::new(writer))),
            exit_on_broken_pipe: true,
        }
    }

    /// Create a logger backed by a custom writer and control whether to exit on broken pipe.
    pub fn with_writer_and_exit_on_broken_pipe(
        writer: Box<dyn Write + Send>,
        exit_on_broken_pipe: bool,
    ) -> Self {
        Self {
            output: LoggerOutput::Writer(Arc::new(Mutex::new(writer))),
            exit_on_broken_pipe,
        }
    }

    /// Set whether the logger will call `std::process::exit(0)` on `BrokenPipe`.
    pub fn set_exit_on_broken_pipe(&mut self, v: bool) {
        self.exit_on_broken_pipe = v;
    }

    /// Low level logging function. Accepts a `fmt::Arguments` so callers can use
    /// `format_args!` and avoid allocating strings when the logger is Noop.
    pub fn log(&self, _level: LogLevel, args: fmt::Arguments) -> io::Result<()> {
        match &self.output {
            LoggerOutput::Noop => {
                // Important: do not call `write_fmt` or otherwise consume `args`.
                // `format_args!` constructs an `Arguments` value cheaply; actual
                // formatting happens only when `write_fmt` is called.
                Ok(())
            }
            LoggerOutput::Stdout => {
                let mut out = io::stdout().lock();
                match out.write_fmt(args) {
                    Ok(()) => match out.write_all(b"\n") {
                        Ok(()) => Ok(()),
                        Err(e) => Self::handle_write_error(e, self.exit_on_broken_pipe),
                    },
                    Err(e) => Self::handle_write_error(e, self.exit_on_broken_pipe),
                }
            }
            LoggerOutput::Stderr => {
                let mut err = io::stderr().lock();
                match err.write_fmt(args) {
                    Ok(()) => match err.write_all(b"\n") {
                        Ok(()) => Ok(()),
                        Err(e) => Self::handle_write_error(e, self.exit_on_broken_pipe),
                    },
                    Err(e) => Self::handle_write_error(e, self.exit_on_broken_pipe),
                }
            }
            LoggerOutput::Writer(mw) => {
                let mut guard = mw
                    .lock()
                    .map_err(|_| io::Error::other("writer mutex poisoned"))?;
                match guard.write_fmt(args) {
                    Ok(()) => match guard.write_all(b"\n") {
                        Ok(()) => Ok(()),
                        Err(e) => Self::handle_write_error(e, self.exit_on_broken_pipe),
                    },
                    Err(e) => Self::handle_write_error(e, self.exit_on_broken_pipe),
                }
            }
        }
    }

    /// Convenience wrappers for levels.
    pub fn info(&self, args: fmt::Arguments) -> io::Result<()> {
        self.log(LogLevel::Info, args)
    }
    pub fn warn(&self, args: fmt::Arguments) -> io::Result<()> {
        self.log(LogLevel::Warn, args)
    }
    pub fn error(&self, args: fmt::Arguments) -> io::Result<()> {
        self.log(LogLevel::Error, args)
    }
    pub fn debug(&self, args: fmt::Arguments) -> io::Result<()> {
        self.log(LogLevel::Debug, args)
    }

    /// Internal error handling abstraction.
    fn handle_write_error(e: io::Error, exit_on_broken_pipe: bool) -> io::Result<()> {
        if e.kind() == ErrorKind::BrokenPipe {
            if exit_on_broken_pipe {
                // Typical CLI semantics: when the reader of the pipe disappears,
                // exit quietly with code 0.
                std::process::exit(0);
            } else {
                // For tests we may prefer not to exit; treat as silent success.
                return Ok(());
            }
        }
        Err(e)
    }
}

/// Macro conveniences so call sites can write:
/// log_info!(logger, "value = {}", x);
#[macro_export]
macro_rules! log_info {
    ($logger:expr, $($arg:tt)*) => {
        { let _ = $logger.info(format_args!($($arg)*)); }
    };
}

#[macro_export]
macro_rules! log_warn {
    ($logger:expr, $($arg:tt)*) => {
        { let _ = $logger.warn(format_args!($($arg)*)); }
    };
}

#[macro_export]
macro_rules! log_error {
    ($logger:expr, $($arg:tt)*) => {
        { let _ = $logger.error(format_args!($($arg)*)); }
    };
}

#[macro_export]
macro_rules! log_debug {
    ($logger:expr, $($arg:tt)*) => {
        { let _ = $logger.debug(format_args!($($arg)*)); }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind, Result as IoResult};

    /// A simple writer that appends bytes into an Arc<Mutex<Vec<u8>>> for inspection.
    struct VecWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl VecWriter {
        fn new() -> (Self, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
            let a = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            (VecWriter(a.clone()), a)
        }
    }

    impl Write for VecWriter {
        fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
            let mut g = self
                .0
                .lock()
                .map_err(|_| std::io::Error::other("VecWriter mutex poisoned".to_string()))?;
            g.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    /// A writer that always returns BrokenPipe error.
    struct BrokenPipeWriter;

    impl Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> IoResult<usize> {
            Err(Error::new(ErrorKind::BrokenPipe, "broken pipe (simulated)"))
        }
        fn flush(&mut self) -> IoResult<()> {
            Err(Error::new(ErrorKind::BrokenPipe, "broken pipe (simulated)"))
        }
    }

    use std::fmt::{Display, Formatter};

    /// A value that panics if formatted. Useful to assert that formatting is not
    /// performed for Noop logger.
    struct PanicOnFormat;

    impl Display for PanicOnFormat {
        fn fmt(&self, _f: &mut Formatter<'_>) -> fmt::Result {
            panic!("format should not be called for Noop");
        }
    }

    #[test]
    fn test_writer_receives_output() {
        let (vw, shared) = VecWriter::new();
        let logger = Logger::with_writer(Box::new(vw));
        // Use the macro convenience
        log_info!(logger, "hello {} {}", "world", 123);
        let g = shared.lock().unwrap();
        let s = String::from_utf8_lossy(&g[..]);
        assert!(s.contains("hello world 123"));
    }

    #[test]
    fn test_noop_avoids_formatting() {
        let logger = Logger::new_noop();
        // This would panic if formatting is attempted; using format_args! will
        // not actually call Display::fmt unless write_fmt is invoked.
        logger.info(format_args!("{}", PanicOnFormat)).unwrap();
    }

    #[test]
    fn test_broken_pipe_handled_without_exit_when_disabled() {
        let logger = Logger::with_writer_and_exit_on_broken_pipe(Box::new(BrokenPipeWriter), false);
        // Should not return an error (treated as silent-success).
        let res = logger.info(format_args!("hi"));
        assert!(res.is_ok());
    }

    #[test]
    fn test_broken_pipe_error_propagates_when_not_broken_pipe() {
        // Create a writer that returns a different error kind to ensure it propagates.
        struct ErrWriter;
        impl Write for ErrWriter {
            fn write(&mut self, _buf: &[u8]) -> IoResult<usize> {
                Err(std::io::Error::other("other".to_string()))
            }
            fn flush(&mut self) -> IoResult<()> {
                Ok(())
            }
        }

        let logger = Logger::with_writer_and_exit_on_broken_pipe(Box::new(ErrWriter), false);
        let res = logger.info(format_args!("hi"));
        assert!(res.is_err());
    }
}
